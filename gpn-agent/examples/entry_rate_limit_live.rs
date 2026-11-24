//! entry_rate_limit_live — prove item 251's per-fingerprint cooldown ON THE LIVE
//! CHAIN, end to end, without waiting 24 h.
//!
//! The acceptance for #251 is: "a machine whose fingerprint was seen before can
//! register again, with no second proof of work, at most once per
//! `FreeEntryCooldown` (5 760 blocks)". Two halves, and only one is observable
//! inside a single run:
//!
//!   1. a *fresh* fingerprint's first free entry still has to mine (the gate did
//!      not become a free-for-all) and writes
//!      `FreeEntryGpuLastEntry[fingerprint] = now`;
//!   2. a second account presenting the *same* fingerprint inside the cooldown is
//!      refused with its own reason `FreeEntryGpuRateLimited` →
//!      `InvalidTransaction::Custom(6)`. The proof it carries is deliberately
//!      bogus (zero nonce / zero hashes): if the pallet ever fell back to PoW
//!      verification this call would answer `Custom(1)`, so the code that comes
//!      back tells the two paths apart.
//!
//! Between (1) and (2) the first account must release the fingerprint
//! (`withdraw_stake`), otherwise the ownership check answers `Custom(4)`
//! (`FreeEntryGpuUsed`) before the cooldown is ever reached.
//!
//! `withdraw_stake` pays a fee, so the first account is funded from the sudo key
//! (the only funded key this host holds) first.
//!
//! Run: `cargo run --release --example entry_rate_limit_live -- ws://127.0.0.1:9944 [threads] [gflops] [sudo_phrase_file]`

use gpn_agent::chain::signer::{self, GpnConfig, Sr25519Signer};
use gpn_agent::mining::search_solution;
use gpn_agent::onboard::{register_provider_entry_unsigned_call, EntryParams, EntryPow};
use sp_core::{sr25519, Pair as _};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use subxt::dynamic::Value;
use subxt::tx::Payload;
use subxt::tx::Signer as _;
use subxt::OnlineClient;

async fn rpc(http: &str, method: &str, params: serde_json::Value) -> serde_json::Value {
    let body = serde_json::json!({"id":1,"jsonrpc":"2.0","method":method,"params":params});
    reqwest::Client::new()
        .post(http)
        .json(&body)
        .send()
        .await
        .expect("rpc send")
        .json()
        .await
        .expect("rpc json")
}

async fn block_number(http: &str) -> u64 {
    let h = rpc(http, "chain_getHeader", serde_json::json!([])).await;
    u64::from_str_radix(
        h["result"]["number"]
            .as_str()
            .expect("header number")
            .trim_start_matches("0x"),
        16,
    )
    .expect("block number")
}

type Storage = subxt::storage::Storage<GpnConfig, OnlineClient<GpnConfig>>;

async fn get(at: &Storage, pallet: &str, entry: &str, params: Vec<Value>) -> Option<Vec<u8>> {
    let q = subxt::dynamic::storage(pallet, entry, params);
    match at.fetch(&q).await {
        Ok(Some(v)) => Some(v.encoded().to_vec()),
        Ok(None) => None,
        Err(e) => {
            println!("  storage {pallet}.{entry} fetch error: {e}");
            None
        }
    }
}

/// Storage view at the **best** head. `at_latest()` follows the finalized head,
/// which on this PoW testnet is stuck at a block whose state the node has already
/// discarded (`UnknownBlock: State already discarded`), so every read must pin the
/// best-head hash explicitly.
async fn at_best(
    client: &OnlineClient<GpnConfig>,
    http: &str,
) -> Result<subxt::storage::Storage<GpnConfig, OnlineClient<GpnConfig>>, Box<dyn std::error::Error>>
{
    let h = rpc(http, "chain_getBlockHash", serde_json::json!([])).await;
    let hash: subxt::utils::H256 = h["result"].as_str().unwrap_or_default().parse()?;
    Ok(client.storage().at(hash))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "ws://127.0.0.1:9944".to_string());
    let threads: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(16);
    let gflops: u64 = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    let phrase_file = std::env::args()
        .nth(4)
        .unwrap_or_else(|| "governance-keys.txt".to_string());
    let http = endpoint
        .replacen("ws://", "http://", 1)
        .replacen("wss://", "https://", 1);

    let client: OnlineClient<GpnConfig> = signer::connect(&endpoint).await?;
    let meta = client.metadata();

    // ── live difficulty ────────────────────────────────────────────────────
    let at = at_best(&client, &http).await?;
    let dbytes = get(&at, "GPUPoW", "Difficulty", vec![])
        .await
        .expect("GPUPoW.Difficulty (is GPUPoW live?)");
    let target = sp_core::U256::from_little_endian(&dbytes);
    println!(
        "live Difficulty = 0x{} (~{:.1}M hashes expected)",
        hex::encode(&dbytes),
        2f64.powf(256.0 - target.bits() as f64) / 1e6
    );
    let start_block = block_number(&http).await;
    println!("head #{start_block}");

    // ── fresh account A and a fingerprint nobody has used ──────────────────
    let mut seed = Vec::new();
    seed.extend(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
            .to_le_bytes(),
    );
    seed.extend(std::process::id().to_le_bytes());
    seed.extend(b"rate-limit-251");
    let seed_a = sp_core::hashing::blake2_256(&seed);
    let gpu_info_hash = sp_core::hashing::blake2_256(&[&seed[..], b"gpu"].concat());
    let pair_a = sr25519::Pair::from_seed(&seed_a);
    let account_a = pair_a.public().0;
    println!("account A       = 0x{}", hex::encode(account_a));
    println!("gpu_info_hash   = 0x{}", hex::encode(gpu_info_hash));

    let fp = vec![Value::from_bytes(gpu_info_hash)];
    println!(
        "GpuOwner[f] before = {:?} (expect None — fresh fingerprint)",
        get(&at, "GPUAttestation", "GpuOwner", fp.clone()).await
    );
    println!(
        "FreeEntryGpuLastEntry[f] before = {:?} (expect None)",
        get(&at, "GPUAttestation", "FreeEntryGpuLastEntry", fp.clone()).await
    );

    // ── mine + submit the first free entry, retrying on a mid-flight retarget ──
    // A proof mined against difficulty D is invalid once the chain retargets: the
    // pool drops it and nothing lands. Row 244's failure mode, met live on
    // 2026-11-23 (two attempts died this way). So each attempt re-reads the
    // difficulty, mines, submits and waits a bounded time before giving up.
    let params = EntryParams {
        challenge_response: [9u8; 32],
        gpu_info_hash,
        gpu_count: 1,
        compute_gflops: gflops,
        requirements: Default::default(),
    };

    let mut recorded_at = None;
    let mut last_hash = String::new();
    for attempt in 1..=5 {
        let at = at_best(&client, &http).await?;
        let dbytes = get(&at, "GPUPoW", "Difficulty", vec![])
            .await
            .expect("GPUPoW.Difficulty");
        let target = sp_core::U256::from_little_endian(&dbytes);
        let head = block_number(&http).await;
        println!(
            "\n[1.{attempt}] difficulty 0x{} (~{:.1}M hashes), head #{head} (next retarget ~#{})",
            hex::encode(&dbytes),
            2f64.powf(256.0 - target.bits() as f64) / 1e6,
            ((head / 45) + 1) * 45
        );

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
        let t0 = std::time::Instant::now();
        let mut seal = None;
        for h in handles {
            if let Ok(Some(s)) = h.join() {
                seal = Some(s);
            }
        }
        let seal = seal.expect("mined a solution");
        println!(
            "[1.{attempt}] mined in {:.1}s ({} hashes) nonce={:#x}",
            t0.elapsed().as_secs_f64(),
            hashes.load(Ordering::Relaxed),
            seal.nonce
        );

        let pow = EntryPow::from_mined(&seal);
        let call = register_provider_entry_unsigned_call(&account_a, &pow, &params);
        let unsigned = client.tx().create_unsigned(&call)?;
        let submit = rpc(
            &http,
            "author_submitExtrinsic",
            serde_json::json!([format!("0x{}", hex::encode(unsigned.encoded()))]),
        )
        .await;
        println!("[1.{attempt}] submit ⇒ {submit}");
        last_hash = submit["result"].as_str().unwrap_or("<refused>").to_string();

        for _ in 0..14 {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            let at = at_best(&client, &http).await?;
            if get(&at, "GPUAttestation", "FreeEntryGpuLastEntry", fp.clone())
                .await
                .is_some()
            {
                recorded_at = Some(block_number(&http).await);
                break;
            }
        }
        if recorded_at.is_some() {
            break;
        }
        println!("[1.{attempt}] not included (tx {last_hash}) — retrying with fresh difficulty");
    }
    let recorded_at = match recorded_at {
        Some(n) => n,
        None => {
            println!("!! the free entry never landed after 5 attempts; stop");
            return Ok(());
        }
    };
    println!("[1] FreeEntryGpuLastEntry[f] written — seen at head #{recorded_at}");

    // ── fund A, then release the fingerprint (fee-paying call) ────────────
    let phrase = std::fs::read_to_string(&phrase_file)?
        .lines()
        .find(|l| l.starts_with("sudo\t"))
        .and_then(|l| l.split('\t').nth(1))
        .map(|s| s.trim().to_string())
        .ok_or("no sudo phrase in governance-keys.txt")?;
    let sudo = Sr25519Signer::from_uri(&phrase)?;
    println!("\n[f] funding A from sudo {}", sudo.account_id());
    let tx = subxt::dynamic::tx(
        "Balances",
        "transfer_keep_alive",
        vec![Value::from_bytes(account_a), Value::u128(1_000_000_000_000)],
    );
    let nonce = signer::current_nonce(&client, &endpoint, &sudo.account_id()).await;
    signer::submit_signed(&client, &tx, &sudo, nonce).await?;
    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
    let at = at_best(&client, &http).await?;
    println!(
        "[f] A account after funding = {:?}",
        get(&at, "System", "Account", vec![Value::from_bytes(account_a)])
            .await
            .map(hex::encode)
    );

    let withdraw = subxt::dynamic::tx("GPUAttestation", "withdraw_stake", Vec::<Value>::new());
    let signer_a = Sr25519Signer::from_pair(pair_a.clone());
    let nonce = signer::current_nonce(&client, &endpoint, &signer_a.account_id()).await;
    signer::submit_signed(&client, &withdraw, &signer_a, nonce).await?;
    println!("[2] A withdrew — fingerprint released");
    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
    let at = at_best(&client, &http).await?;
    println!(
        "[2] GpuOwner[f] after withdraw = {:?} (expect None)",
        get(&at, "GPUAttestation", "GpuOwner", fp.clone()).await
    );
    println!(
        "[2] FreeEntryGpuLastEntry[f] still set = {:?} (the cooldown clock keeps running)",
        get(&at, "GPUAttestation", "FreeEntryGpuLastEntry", fp.clone()).await
    );

    // ── 3) second, fresh account presents the same fingerprint with a
    //       deliberately bogus proof: only the cooldown can answer this ─────
    let seed_b = sp_core::hashing::blake2_256(&[&seed[..], b"account-b"].concat());
    let pair_b = sr25519::Pair::from_seed(&seed_b);
    let account_b = pair_b.public().0;
    println!("\n[3] account B = 0x{}", hex::encode(account_b));
    let bogus = EntryPow {
        nonce: 0,
        mix_hash: [0u8; 32],
        solution_hash: [0u8; 32],
        algorithm_version: 0,
    };
    let call_b = register_provider_entry_unsigned_call(&account_b, &bogus, &params);
    let unsigned_b = client.tx().create_unsigned(&call_b)?;
    let submit_b = rpc(
        &http,
        "author_submitExtrinsic",
        serde_json::json!([format!("0x{}", hex::encode(unsigned_b.encoded()))]),
    )
    .await;
    println!("[3] submit result = {submit_b}");
    let data = submit_b["error"]["data"].as_str().unwrap_or("<none>");
    let msg = submit_b["error"]["message"].as_str().unwrap_or("<none>");
    println!("[3] data={data:?} message={msg:?}   (Custom error: 6 = FreeEntryGpuRateLimited)");
    let now = block_number(&http).await;
    if data.contains("Custom error: 6") {
        println!(
            "\nPASS: item 251 — a previously-used fingerprint, released and re-presented {} blocks after its free entry (cooldown 5 760), is refused with its own reason `Custom error: 6` (FreeEntryGpuRateLimited), not `Custom error: 1` (proof) and not `Custom error: 4` (owner)",
            now.saturating_sub(recorded_at)
        );
    } else {
        println!("\nFAIL: expected `Custom error: 6`; got {data:?}");
    }
    Ok(())
}
