//! entry_live_register — prove (or refute) the unsigned zero-balance entry on the LIVE chain.
//!
//! Row 242: the 5th box mined a valid entry proof and its submission died with
//! `entry-submit-rejected: {"code":1010,"data":"Transaction has a bad signature"}`.
//! Row 243 showed that message is `InvalidTransaction::BadProof` from `precheck_pow`,
//! i.e. the proof itself was refused, not the encoding.
//!
//! This example removes every avoidable variable: it mines a *fresh* proof against the
//! live `GPUPoW::Difficulty`, for an account that has never existed, with a GPU
//! fingerprint nobody has used, and reports:
//!   * the resolved pallet/call index and the exact unsigned hex (as `GPN_TRACE_TX=1` would),
//!   * the RPC answer verbatim (error line if the pool refuses),
//!   * the inclusion block and the extrinsic's index in it, or a plain statement that
//!     it was not included.
//!
//! Run: `cargo run --release --example entry_live_register -- ws://127.0.0.1:9944 [threads] [gflops]`
//! `gflops` < 250 (< MinEligibilityGflops) makes validate_unsigned pass while the dispatch
//! refuses with BelowEligibilityFloor — useful to prove pool admission without registering.

use gpn_agent::chain::signer;
use gpn_agent::mining::search_solution;
use gpn_agent::onboard::{register_provider_entry_unsigned_call, EntryParams, EntryPow};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use subxt::tx::Payload;

fn storage_key(pallet: &str, item: &str, key: Option<&[u8]>) -> String {
    let mut out = Vec::new();
    out.extend(twox128(pallet.as_bytes()));
    out.extend(twox128(item.as_bytes()));
    if let Some(k) = key {
        out.extend(blake2_128_concat(k));
    }
    format!("0x{}", hex::encode(out))
}

fn twox128(d: &[u8]) -> Vec<u8> {
    sp_core::hashing::twox_128(d).to_vec()
}

fn blake2_128_concat(k: &[u8]) -> Vec<u8> {
    let h = sp_core::hashing::blake2_128(k);
    let mut v = h.to_vec();
    v.extend(k);
    v
}

async fn rpc(http: &str, method: &str, params: serde_json::Value) -> serde_json::Value {
    let body = serde_json::json!({"id":1,"jsonrpc":"2.0","method":method,"params":params});
    let resp: serde_json::Value = reqwest::Client::new()
        .post(http)
        .json(&body)
        .send()
        .await
        .expect("rpc transport")
        .json()
        .await
        .expect("rpc json");
    resp
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "ws://127.0.0.1:9944".to_string());
    let threads: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);
    let gflops: u64 = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    let http = endpoint
        .replacen("ws://", "http://", 1)
        .replacen("wss://", "https://", 1);

    let client = signer::connect(&endpoint).await?;
    let meta = client.metadata();

    // ── live difficulty ────────────────────────────────────────────────────
    let dkey = storage_key("GPUPoW", "Difficulty", None);
    let dval = rpc(&http, "state_getStorage", serde_json::json!([dkey])).await;
    let dhex = dval["result"].as_str().expect("difficulty storage");
    let dbytes = hex::decode(dhex.trim_start_matches("0x"))?;
    let target = sp_core::U256::from_little_endian(&dbytes);
    let bits = target.bits() as f64;
    println!(
        "live Difficulty = 0x{}  (~2^{:.1} → {:.0}M hashes expected)",
        dhex.trim_start_matches("0x"),
        256.0 - bits,
        2f64.powf(256.0 - bits) / 1e6
    );

    // Blocks until the next retarget (ADJUST window 45) — a move mid-mine is row 244.
    let head = rpc(&http, "chain_getHeader", serde_json::json!([])).await;
    let number = u64::from_str_radix(
        head["result"]["number"]
            .as_str()
            .unwrap()
            .trim_start_matches("0x"),
        16,
    )?;
    println!(
        "head #{number} (next retarget at #{} → {} blocks away)",
        ((number / 45) + 1) * 45,
        (((number / 45) + 1) * 45) - number
    );

    // ── a fresh account and a fresh GPU fingerprint ─────────────────────────
    let mut seed = Vec::new();
    seed.extend(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
            .to_le_bytes(),
    );
    seed.extend(std::process::id().to_le_bytes());
    let account = sp_core::hashing::blake2_256(&seed);
    let gpu_info_hash = sp_core::hashing::blake2_256(&[&seed[..], b"gpu"].concat());
    println!("fresh account  = 0x{}", hex::encode(account));
    println!("gpu_info_hash  = 0x{}", hex::encode(gpu_info_hash));

    // ── mine ───────────────────────────────────────────────────────────────
    let stop = Arc::new(AtomicBool::new(false));
    let hashes = Arc::new(AtomicU64::new(0));
    let mut handles = Vec::new();
    for t in 0..threads {
        let stop = stop.clone();
        let hashes = hashes.clone();
        let start = (t as u64) << 56;
        handles.push(std::thread::spawn(move || {
            let batch = 1usize << 16;
            let mut nonce = start;
            loop {
                if stop.load(Ordering::Relaxed) {
                    return None;
                }
                if let Some(seal) = search_solution(&target, nonce, batch, &stop) {
                    stop.store(true, Ordering::SeqCst);
                    return Some(seal);
                }
                nonce = nonce.wrapping_add(batch as u64);
                hashes.fetch_add(batch as u64, Ordering::Relaxed);
            }
        }));
    }
    let reporter = {
        let hashes = hashes.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            let t0 = std::time::Instant::now();
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_secs(5));
                let h = hashes.load(Ordering::Relaxed);
                println!(
                    "  … {:.0}M hashes, {:.1} MH/s, {}s",
                    h as f64 / 1e6,
                    h as f64 / t0.elapsed().as_secs_f64() / 1e6,
                    t0.elapsed().as_secs()
                );
            }
        })
    };
    let mut found = None;
    for h in handles {
        if let Ok(Some(seal)) = h.join() {
            found = Some(seal);
        }
    }
    let _ = reporter.join();
    let seal = found.expect("mined a solution");

    let pow = EntryPow::from_mined(&seal);
    println!(
        "mined proof: nonce={:#x} mix={} solution={}",
        pow.nonce,
        hex::encode(&pow.mix_hash[..4]),
        hex::encode(&pow.solution_hash[..6])
    );
    println!("proof meets target (client check) = {}", pow.meets(&target));

    // ── build + submit the unsigned extrinsic ──────────────────────────────
    let params = EntryParams {
        challenge_response: [9u8; 32],
        gpu_info_hash,
        gpu_count: 1,
        compute_gflops: gflops,
        requirements: Default::default(),
    };
    let call = register_provider_entry_unsigned_call(&account, &pow, &params);
    let encoded = call.encode_call_data(&meta)?;
    println!("\ncall-data hex   = 0x{}", hex::encode(&encoded));
    println!(
        "  pallet={} call={} (source: ComputeMarketplace / register_provider_entry_unsigned)",
        encoded[0], encoded[1]
    );

    let unsigned = client.tx().create_unsigned(&call)?;
    let bytes = unsigned.encoded();
    let hexstr = format!("0x{}", hex::encode(bytes));
    println!("unsigned hex    = {hexstr}");
    println!(
        "  version={:#04x} pallet={} call={} len={}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes.len()
    );

    let submit = rpc(&http, "author_submitExtrinsic", serde_json::json!([hexstr])).await;
    println!("\nsubmit result   = {submit}");
    if let Some(err) = submit.get("error") {
        println!("POOL REFUSED: {err}");
        return Ok(());
    }
    let tx_hash = submit["result"].as_str().unwrap_or_default().to_string();

    // ── inclusion ──────────────────────────────────────────────────────────
    let rkey = storage_key("ComputeMarketplace", "RegisteredProviders", Some(&account));
    let mut included_at = None;
    let mut included_hash: Option<String> = None;
    for i in 0..40 {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let stored = rpc(&http, "state_getStorage", serde_json::json!([rkey.clone()])).await;
        let registered = stored["result"]
            .as_str()
            .map(|s| s != "0x")
            .unwrap_or(false);
        if registered {
            included_at = Some(i);
            break;
        }
        // also scan the current block for our extrinsic (a refused dispatch still lands)
        let h = rpc(&http, "chain_getHeader", serde_json::json!([])).await;
        let num = u64::from_str_radix(
            h["result"]["number"]
                .as_str()
                .unwrap()
                .trim_start_matches("0x"),
            16,
        )?;
        let bh = rpc(&http, "chain_getBlockHash", serde_json::json!([num])).await;
        let block = rpc(&http, "chain_getBlock", serde_json::json!([bh["result"]])).await;
        let exts = block["result"]["block"]["extrinsics"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if let Some(pos) = exts.iter().position(|e| {
            let raw = match e {
                serde_json::Value::Array(a) => a
                    .iter()
                    .filter_map(|x| x.as_u64())
                    .map(|x| x as u8)
                    .collect::<Vec<u8>>(),
                serde_json::Value::String(s) => {
                    hex::decode(s.trim_start_matches("0x")).unwrap_or_default()
                }
                _ => Vec::new(),
            };
            raw == bytes
        }) {
            println!(
                "\nINCLUDED in block #{num} at extrinsic index {pos} (dispatch may have refused)"
            );
            let bhash = bh["result"].as_str().unwrap_or_default().to_string();
            println!("inclusion block hash = {bhash}");
            println!("tx hash = {tx_hash}");
            included_hash = Some(bhash);
            included_at = Some(i);
            break;
        }
    }

    // ── decoded events of the inclusion block ──────────────────────────────
    if let Some(bhash) = included_hash {
        let hash: subxt::config::substrate::H256 = bhash.parse().expect("block hash");
        match client.events().at(hash).await {
            Ok(evs) => {
                println!("\nevents in that block:");
                for ev in evs.iter() {
                    match ev {
                        Ok(e) => {
                            let p = e.pallet_name();
                            let v = e.variant_name();
                            if p == "System" || p == "ComputeMarketplace" || p == "GPUAttestation" {
                                println!("  {p}::{v} {:?}", e.field_values());
                            }
                        }
                        Err(e) => println!("  <event decode error: {e}>"),
                    }
                }
            }
            Err(e) => println!("\nevent fetch failed: {e}"),
        }
    }

    let final_head = rpc(&http, "chain_getHeader", serde_json::json!([])).await;
    let final_num = u64::from_str_radix(
        final_head["result"]["number"]
            .as_str()
            .unwrap()
            .trim_start_matches("0x"),
        16,
    )?;
    let stored = rpc(&http, "state_getStorage", serde_json::json!([rkey])).await;
    match stored["result"].as_str() {
        Some(v) if v != "0x" => println!(
            "\nRESULT: REGISTERED on chain — RegisteredProviders[0x{}] present ({} bytes), head #{final_num}",
            hex::encode(account), (v.len() - 2) / 2),
        _ => println!("\nRESULT: NOT REGISTERED — no RegisteredProviders entry for 0x{} at head #{final_num}{}",
            hex::encode(account),
            if included_at.is_some() { " (extrinsic was included, dispatch refused)" } else { " (extrinsic never included)" }),
    }
    Ok(())
}
