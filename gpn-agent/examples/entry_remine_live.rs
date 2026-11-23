//! entry_remine_live — row 244 on the LIVE chain: a proof the pool refuses
//! because the target moved is re-mined and accepted, through the production
//! `onboard::submit_unsigned_with_remine`.
//!
//! What is proved, in order:
//!   1. a proof that is valid against an *older, easier* target ("the target
//!      before the retarget") is refused by the live pool,
//!   2. the production retry path re-reads the live difficulty, re-mines against
//!      it, logs `re-mined against the newer target` with both targets and both
//!      nonces, and
//!   3. the resubmission is ACCEPTED and included on chain.
//!
//! The stale target is synthesised as `live_target * factor` (4 by default): the
//! 45-block retarget cannot be scheduled on demand, and a proof mined against a
//! target 4× easier is *exactly* the state the client is in after a hard move.
//! No existing provider is touched: the account is fresh entropy, the GPU
//! fingerprint is fresh, and nothing signs anything.
//!
//! Run: `cargo run --release --example entry_remine_live -- ws://127.0.0.1:9944 [factor] [threads]`

use gpn_agent::chain::signer;
use gpn_agent::mining::search_solution;
use gpn_agent::onboard::{
    register_provider_entry_unsigned_call, submit_unsigned_with_remine, EntryChain, EntryOutcome,
    EntryParams, EntryPow, EntryStatus, PowMiner,
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

fn storage_key(pallet: &str, item: &str) -> String {
    let mut out = Vec::new();
    out.extend(sp_core::hashing::twox_128(pallet.as_bytes()).to_vec());
    out.extend(sp_core::hashing::twox_128(item.as_bytes()).to_vec());
    format!("0x{}", hex::encode(out))
}

async fn rpc(http: &str, method: &str, params: serde_json::Value) -> serde_json::Value {
    let body = serde_json::json!({"id":1,"jsonrpc":"2.0","method":method,"params":params});
    reqwest::Client::new()
        .post(http)
        .json(&body)
        .send()
        .await
        .expect("rpc transport")
        .json()
        .await
        .expect("rpc json")
}

/// Live chain adapter: unsigned submission through the same `signer` plumbing
/// as `LiveEntryChain`, but for an account that has no key (the whole point of
/// the unsigned entry).
struct FreshAccountChain {
    endpoint: String,
    http: String,
    account: [u8; 32],
}

impl EntryChain for FreshAccountChain {
    async fn status(&self, _account: &str) -> Result<EntryStatus, String> {
        Ok(EntryStatus::default())
    }

    async fn difficulty(&self) -> Result<sp_core::U256, String> {
        let dkey = storage_key("GPUPoW", "Difficulty");
        let dval = rpc(&self.http, "state_getStorage", serde_json::json!([dkey])).await;
        let dhex = dval["result"]
            .as_str()
            .ok_or_else(|| format!("difficulty storage missing: {dval}"))?;
        let dbytes = hex::decode(dhex.trim_start_matches("0x")).map_err(|e| e.to_string())?;
        let target = sp_core::U256::from_little_endian(&dbytes);
        if target.is_zero() {
            return Err("chain difficulty target is zero — is the node synced?".into());
        }
        Ok(target)
    }

    async fn submit_register_provider_entry(
        &self,
        _pow: &EntryPow,
        _params: &EntryParams,
    ) -> Result<(), String> {
        Err("signed entry not available on a keyless account".into())
    }

    async fn submit_register_provider_entry_unsigned(
        &self,
        pow: &EntryPow,
        params: &EntryParams,
    ) -> Result<(), String> {
        let call = register_provider_entry_unsigned_call(&self.account, pow, params);
        let client = signer::connect(&self.endpoint)
            .await
            .map_err(|e| format!("connect: {e}"))?;
        signer::submit_unsigned(&client, &self.endpoint, &call)
            .await
            .map_err(|e| e.to_string())
    }

    async fn submit_attest_entry(
        &self,
        _pow: &EntryPow,
        _params: &EntryParams,
    ) -> Result<(), String> {
        Err("attest_entry not available on a keyless account".into())
    }

    async fn submit_register_provider(&self, _params: &EntryParams) -> Result<(), String> {
        Err("register_provider is a signed call".into())
    }
}

/// Mines a proof that clears `stale` but NOT `live` — i.e. the proof a client
/// holds when the target moved under it. Multi-threaded scan, no GPU.
struct StaleMiner {
    stale: sp_core::U256,
    live: sp_core::U256,
    threads: usize,
}

impl PowMiner for StaleMiner {
    fn mine(&self, target: sp_core::U256, log: &mut dyn FnMut(&str)) -> Result<EntryPow, String> {
        // Row 244's re-mine: mine against the target handed in (the live one).
        let stop = Arc::new(AtomicBool::new(false));
        let hashes = Arc::new(AtomicU64::new(0));
        let mut handles = Vec::new();
        for t in 0..self.threads.max(1) {
            let stop = stop.clone();
            let hashes = hashes.clone();
            let start = (t as u64 + 1) << 56;
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
        let mut found = None;
        for h in handles {
            if let Ok(Some(seal)) = h.join() {
                found = Some(seal);
            }
        }
        let seal = found.ok_or("proof search was cancelled")?;
        log(&format!(
            "  re-mine: {} hashes searched, nonce={:#x}",
            hashes.load(Ordering::Relaxed),
            seal.nonce
        ));
        Ok(EntryPow::from_mined(&seal))
    }
}

impl StaleMiner {
    fn mine_stale(&self, log: &mut dyn FnMut(&str)) -> Result<EntryPow, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let mut nonce = 0u64;
        let mut searched = 0u64;
        loop {
            let batch = 1usize << 22;
            match search_solution(&self.stale, nonce, batch, &stop) {
                Some(seal) => {
                    searched += batch as u64;
                    let pow = EntryPow::from_mined(&seal);
                    // The hash is little-endian: compare as a number, not as bytes.
                    if sp_core::U256::from_little_endian(&pow.solution_hash) >= self.live {
                        log(&format!(
                            "  stale proof: nonce={:#x} hash {} clears the old target but NOT the live one ({} hashes searched)",
                            pow.nonce,
                            hex::encode(&pow.solution_hash[..6]),
                            searched
                        ));
                        return Ok(pow);
                    }
                    nonce = seal.nonce.wrapping_add(1);
                }
                None => {
                    if stop.load(Ordering::Relaxed) {
                        return Err("stale proof search was cancelled".into());
                    }
                    // No solution in this window: keep scanning, not a failure.
                    nonce = nonce.wrapping_add(batch as u64);
                    searched += batch as u64;
                }
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "ws://127.0.0.1:9944".to_string());
    let factor: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);
    let threads: usize = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);
    let http = endpoint.replacen("ws://", "http://", 1);

    let live = {
        let dkey = storage_key("GPUPoW", "Difficulty");
        let dval = rpc(&http, "state_getStorage", serde_json::json!([dkey])).await;
        let dhex = dval["result"].as_str().expect("difficulty storage");
        sp_core::U256::from_little_endian(&hex::decode(dhex.trim_start_matches("0x"))?)
    };
    let stale = live * sp_core::U256::from(factor);
    println!(
        "live target  ~2^{:.1}   stale target ~2^{:.1} (×{factor} easier — 'before the retarget')",
        256.0 - live.bits() as f64,
        256.0 - stale.bits() as f64
    );

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
    println!("fresh account = 0x{}", hex::encode(account));

    let params = EntryParams {
        challenge_response: [9u8; 32],
        gpu_info_hash,
        gpu_count: 1,
        compute_gflops: 300,
        requirements: Default::default(),
    };
    let miner = StaleMiner {
        stale,
        live,
        threads,
    };
    let mut say = |s: &str| println!("{s}");
    let pow = miner.mine_stale(&mut say)?;

    let chain = FreshAccountChain {
        endpoint: endpoint.clone(),
        http: http.clone(),
        account,
    };
    let outcome = submit_unsigned_with_remine(&chain, &miner, pow, stale, &params, &mut say).await;
    println!("\noutcome = {outcome:?}");

    // Confirm the account is a provider on chain now.
    let key = {
        let mut k = Vec::new();
        k.extend(sp_core::hashing::twox_128(b"ComputeMarketplace").to_vec());
        k.extend(sp_core::hashing::twox_128(b"RegisteredProviders").to_vec());
        k.extend(sp_core::hashing::blake2_128(&account).to_vec());
        k.extend(account);
        format!("0x{}", hex::encode(k))
    };
    let stored = rpc(&http, "state_getStorage", serde_json::json!([key])).await;
    let on_chain = stored["result"].is_string();
    println!(
        "RegisteredProviders[0x{}] present = {on_chain}",
        hex::encode(account)
    );

    match outcome {
        EntryOutcome::Registered if on_chain => {
            println!("RESULT: PASS — refused proof re-mined against the live target and accepted");
            Ok(())
        }
        other => {
            println!("RESULT: FAIL — outcome {other:?}, on_chain={on_chain}");
            std::process::exit(1);
        }
    }
}
