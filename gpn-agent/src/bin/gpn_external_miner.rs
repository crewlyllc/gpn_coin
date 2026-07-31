//! External GPU miner for GPN — wires the `gpn-agent` OpenCL Ethash miner to
//! the node's external-mining RPC (`gpn_getTemplate` / `gpn_submitWork`).
//!
//! This completes the **production mining architecture**: the node and the
//! miner are now fully separable. This process talks to a lightweight node
//! (which needs no internal self-miner) purely over JSON-RPC, mines on its own
//! GPU, and submits the finished seal back over the wire. It can run on a
//! machine that does not run a full node at all.
//!
//! This is also the substrate for the headline **GPU auto-switch** feature:
//! the same GPU can spend one period serving AI compute and the next mining,
//! because mining is just an HTTP call away from the node.
//!
//! Usage:
//!   gpn_external_miner [rpc_url] [device_index] [batch]
//!
//! Defaults: rpc_url = http://127.0.0.1:9944, device_index = 0, batch = 1<<20

use gpn_agent::gpu::ethash_miner::EthashGpuMiner;
use serde_json::{json, Value};
use sp_core::U256;
use std::time::{Duration, Instant};

/// JSON-RPC client over HTTP for the GPN node's external-mining API.
struct MinerRpc {
    url: String,
    client: reqwest::Client,
    next_id: u64,
}

#[derive(Debug)]
struct MiningTemplate {
    pre_hash: [u8; 32],
    difficulty: String, // big-endian hex from the node (U256 display)
    block_number: u32,
    best_hash: String,
}

impl MinerRpc {
    fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
            client: reqwest::Client::new(),
            next_id: 1,
        }
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let resp = self
            .client
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("rpc {method} transport: {e}"))?;
        let v: Value = resp.json().await.map_err(|e| format!("rpc {method} decode: {e}"))?;
        if let Some(err) = v.get("error") {
            return Err(format!("rpc {method} error: {err}"));
        }
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Fetch the current mining template, or None if the node has none yet.
    async fn get_template(&mut self) -> Result<Option<MiningTemplate>, String> {
        let r = self.call("gpn_getTemplate", json!([])).await?;
        if r.is_null() {
            return Ok(None);
        }
        let pre_hex = r
            .get("pre_hash")
            .and_then(|v| v.as_str())
            .ok_or("template missing pre_hash")?;
        let difficulty = r
            .get("difficulty")
            .and_then(|v| v.as_str())
            .ok_or("template missing difficulty")?;
        let block_number = r.get("block_number").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let best_hash = r.get("best_hash").and_then(|v| v.as_str()).unwrap_or("").to_string();

        let mut pre_hash = [0u8; 32];
        let bytes = hex::decode(pre_hex.trim_start_matches("0x"))
            .map_err(|e| format!("template pre_hash hex: {e}"))?;
        if bytes.len() != 32 {
            return Err(format!("template pre_hash must be 32 bytes, got {}", bytes.len()));
        }
        pre_hash.copy_from_slice(&bytes);

        Ok(Some(MiningTemplate {
            pre_hash,
            difficulty: difficulty.to_string(),
            block_number,
            best_hash,
        }))
    }

    /// Submit a finished seal (40 bytes: 8 LE nonce ++ 32 mix). Returns true
    /// if the node imported the block, false if it was stale.
    async fn submit_work(&mut self, pre_hash_hex: &str, seal: &[u8]) -> Result<bool, String> {
        let seal_hex = format!("0x{}", hex::encode(seal));
        let r = self
            .call("gpn_submitWork", json!([pre_hash_hex, seal_hex]))
            .await?;
        r.as_bool().ok_or("submitWork result not a bool".to_string())
    }
}

/// Convert a big-endian hex difficulty string (U256 display, e.g. "0x2000…00")
/// into a 32-byte little-endian target array for the GPU kernel, which
/// compares `result(LE) < target(LE)`.
fn difficulty_to_le_target(difficulty: &str) -> [u8; 32] {
    let value = U256::from_str_radix(difficulty.trim_start_matches("0x"), 16)
        .expect("difficulty hex is a valid U256");
    let mut target = [0u8; 32];
    value.to_little_endian(&mut target);
    target
}

fn parse_u64(s: &str, default: u64) -> u64 {
    s.parse().unwrap_or(default)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    let rpc_url = args.get(1).cloned().unwrap_or_else(|| "http://127.0.0.1:9944".to_string());
    let device_index: usize = parse_u64(args.get(2).map(|s| s.as_str()).unwrap_or("0"), 0) as usize;
    let batch: usize = parse_u64(args.get(3).map(|s| s.as_str()).unwrap_or("1048576"), 1 << 20) as usize;
    let poll_ms: u64 = parse_u64(args.get(4).map(|s| s.as_str()).unwrap_or("2000"), 2000);

    println!("== GPN external GPU miner ==");
    println!("  rpc:          {rpc_url}");
    println!("  device index: {device_index}");
    println!("  batch:        {batch}");

    for line in EthashGpuMiner::device_summary() {
        println!("  device: {line}");
    }

    let miner = EthashGpuMiner::new(device_index).map_err(|e| format!("open GPU miner: {e}"))?;
    println!("using GPU: {}", miner.device_name());

    // Build the epoch-0 DAG once; it persists across all blocks in this epoch.
    let epoch = 0usize;
    let cache_size = ethash::get_cache_size(epoch);
    let full_size = ethash::get_full_size(epoch);
    let seed = ethash::get_seedhash(epoch);
    let mut cache = vec![0u8; cache_size];
    ethash::make_cache(&mut cache, seed);
    println!("light cache: {} KiB, full DAG: {} MiB", cache_size / 1024, full_size / (1024 * 1024));
    let dag = miner.build_dag(&cache, full_size)?;

    let mut rpc = MinerRpc::new(&rpc_url);

    // Main mining loop: fetch template → mine → submit → repeat.
    let mut last_block: Option<u32> = None;
    let mut total_hashes: u64 = 0;
    let mut session_start = Instant::now();
    let mut mine_start = Instant::now();
    let mut hashrate_window: u64 = 0;

    loop {
        // 1. Fetch the current template.
        let tmpl = match rpc.get_template().await {
            Ok(Some(t)) => t,
            Ok(None) => {
                println!("[wait] node has no mining template yet — retrying in {poll_ms}ms");
                tokio::time::sleep(Duration::from_millis(poll_ms)).await;
                continue;
            }
            Err(e) => {
                eprintln!("[error] getTemplate: {e} — retrying in {poll_ms}ms");
                tokio::time::sleep(Duration::from_millis(poll_ms)).await;
                continue;
            }
        };

        if last_block != Some(tmpl.block_number) {
            println!(
                "🎯 new template: block #{} best_hash={} pre_hash=0x{} difficulty={}",
                tmpl.block_number,
                tmpl.best_hash,
                hex::encode(&tmpl.pre_hash[..8]),
                tmpl.difficulty
            );
            last_block = Some(tmpl.block_number);
            mine_start = Instant::now();
            hashrate_window = 0;
        }

        let target = difficulty_to_le_target(&tmpl.difficulty);
        let pre_hash_hex = format!("0x{}", hex::encode(&tmpl.pre_hash));

        // 2. Mine on the GPU until a seal below target is found.
        let mut nonce_start: u64 = 0;
        let seal = loop {
            let found = miner
                .mine_batch(&dag, &tmpl.pre_hash, &target, nonce_start, batch)
                .map_err(|e| format!("gpu mine: {e}"))?;
            total_hashes += batch as u64;
            hashrate_window += batch as u64;
            match found {
                Some(s) => break s,
                None => nonce_start = nonce_start.wrapping_add(batch as u64),
            }

            // Report hashrate every ~2s.
            let elapsed = mine_start.elapsed();
            if elapsed.as_secs() >= 2 {
                let mhs = hashrate_window as f64 / elapsed.as_secs_f64() / 1e6;
                println!("  [rate] {:.1} MH/s (block #{})", mhs, tmpl.block_number);
                mine_start = Instant::now();
                hashrate_window = 0;
            }
        };

        let seal_bytes = seal.to_seal_bytes();
        println!(
            "⛏️  mined nonce={:#x} mix=0x{}... result=0x{}...",
            seal.nonce,
            hex::encode(&seal.mix[..4]),
            hex::encode(&seal.result[..4])
        );

        // 3. Submit the seal.
        match rpc.submit_work(&pre_hash_hex, &seal_bytes).await {
            Ok(true) => {
                println!("✅ SUBMITTED block #{} — accepted by node", tmpl.block_number);
            }
            Ok(false) => {
                println!("↩️  stale template (block #{}) — refetching", tmpl.block_number);
            }
            Err(e) => {
                eprintln!("[error] submitWork: {e}");
            }
        }

        // Session hashrate summary.
        let session_elapsed = session_start.elapsed();
        if session_elapsed.as_secs() >= 30 {
            let mhs = total_hashes as f64 / session_elapsed.as_secs_f64() / 1e6;
            println!("  [session] {:.1} MH/s avg over {}s", mhs, session_elapsed.as_secs());
            session_start = Instant::now();
            total_hashes = 0;
        }

        // Refetch immediately; the node has a new build after import.
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
