//! External GPU mining engine — a *stoppable* wrapper around the OpenCL
//! Ethash miner that talks to the node's external-mining RPC
//! (`gpn_getTemplate` / `gpn_submitWork`).
//!
//! This is the production mining substrate for the agent's **auto-switch**
//! feature: the agent can start mining (spawn the loop), and later stop it
//! (cancel the loop and release the GPU DAG from VRAM) to free the GPU for a
//! compute job. Mining is just a startable/stoppable task here, not the whole
//! process.
//!
//! The loop runs on a dedicated OS thread (not a tokio task) because the
//! `EthashGpuMiner` / `DagBuffer` hold raw OpenCL handles that are moved into
//! the thread at spawn time. All network I/O inside the thread uses
//! `reqwest::blocking`, so the thread is self-contained and safe to cancel.

use crate::gpu::ethash_miner::EthashGpuMiner;
use serde_json::{json, Value};
use sp_core::U256;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Configuration for the external mining engine.
#[derive(Clone, Debug)]
pub struct MiningConfig {
    /// Node JSON-RPC URL, e.g. http://127.0.0.1:9944
    pub rpc_url: String,
    /// OpenCL device index (0 = first GPU)
    pub device_index: usize,
    /// Nonces searched per kernel batch
    pub batch: usize,
    /// How long to wait when the node has no template yet
    pub poll_ms: u64,
}

impl MiningConfig {
    pub fn new(rpc_url: &str) -> Self {
        Self {
            rpc_url: rpc_url.to_string(),
            device_index: 0,
            batch: 1 << 20,
            poll_ms: 2000,
        }
    }
}

/// Handle to a running external mining loop. Dropping the handle does **not**
/// stop mining — call [`ExternalMiner::stop`] to cancel the loop and release
/// the GPU.
pub struct ExternalMiner {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// GPU device name, captured at spawn for diagnostics.
    device_name: String,
}

impl ExternalMiner {
    /// Build the epoch-0 DAG once and start the mining loop on a background
    /// thread. Returns immediately; mining proceeds in the background.
    pub fn start(cfg: MiningConfig) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));

        // Enumerate the GPU up front so a missing device fails fast here,
        // before we spawn a thread.
        let miner =
            EthashGpuMiner::new(cfg.device_index).map_err(|e| format!("open GPU miner: {e}"))?;
        let device_name = miner.device_name();

        // Build the epoch-0 DAG once; it persists across all blocks in this epoch.
        let epoch = 0usize;
        let cache_size = ethash::get_cache_size(epoch);
        let full_size = ethash::get_full_size(epoch);
        let seed = ethash::get_seedhash(epoch);
        let mut cache = vec![0u8; cache_size];
        ethash::make_cache(&mut cache, seed);
        log::info!(
            "mining engine: light cache {} KiB, full DAG {} MiB on {device_name}",
            cache_size / 1024,
            full_size / (1024 * 1024)
        );
        let dag = miner
            .build_dag(&cache, full_size)
            .map_err(|e| format!("build DAG: {e}"))?;

        let stop_clone = stop.clone();
        let thread = std::thread::Builder::new()
            .name("gpn-miner".into())
            .spawn(move || {
                mining_loop(miner, dag, cfg, stop_clone);
            })
            .map_err(|e| format!("spawn miner thread: {e}"))?;

        Ok(Self {
            stop,
            thread: Some(thread),
            device_name,
        })
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// True if the loop is still running.
    pub fn is_running(&self) -> bool {
        self.thread
            .as_ref()
            .map(|t| !t.is_finished())
            .unwrap_or(false)
    }

    /// Cancel the mining loop and wait for the thread to exit, releasing the
    /// GPU DAG from VRAM. Bounded wait (max ~5s) so a stuck RPC call cannot
    /// block the caller forever.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !t.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(25));
            }
            if t.is_finished() {
                let _ = t.join();
                log::info!("mining engine stopped — GPU DAG released");
            } else {
                log::warn!("mining engine did not stop within 5s — detaching (GPU may stay busy)");
            }
        }
    }
}

/// The mining loop body. Runs on the dedicated thread.
fn mining_loop(
    miner: EthashGpuMiner,
    dag: crate::gpu::ethash_miner::DagBuffer,
    cfg: MiningConfig,
    stop: Arc<AtomicBool>,
) {
    let mut rpc = BlockingMinerRpc::new(&cfg.rpc_url);

    let mut last_block: Option<u32> = None;
    let mut total_hashes: u64 = 0;
    let mut session_start = Instant::now();
    let mut mine_start = Instant::now();
    let mut hashrate_window: u64 = 0;

    while !stop.load(Ordering::SeqCst) {
        // 1. Fetch the current template.
        let tmpl = match rpc.get_template() {
            Ok(Some(t)) => t,
            Ok(None) => {
                log::debug!(
                    "[wait] node has no mining template yet — retrying in {}ms",
                    cfg.poll_ms
                );
                if wait_interruptible(&stop, cfg.poll_ms) {
                    break;
                }
                continue;
            }
            Err(e) => {
                log::warn!("[error] getTemplate: {e} — retrying in {}ms", cfg.poll_ms);
                if wait_interruptible(&stop, cfg.poll_ms) {
                    break;
                }
                continue;
            }
        };

        if last_block != Some(tmpl.block_number) {
            log::info!(
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
        let pre_hash_hex = format!("0x{}", hex::encode(tmpl.pre_hash));

        // 2. Mine on the GPU until a seal below target is found.
        let mut nonce_start: u64 = 0;
        let seal = loop {
            if stop.load(Ordering::SeqCst) {
                log::info!("mining loop cancelled mid-search — stopping");
                return;
            }
            let found =
                match miner.mine_batch(&dag, &tmpl.pre_hash, &target, nonce_start, cfg.batch) {
                    Ok(f) => f,
                    Err(e) => {
                        log::error!("gpu mine error: {e}");
                        if wait_interruptible(&stop, cfg.poll_ms) {
                            return;
                        }
                        continue;
                    }
                };
            total_hashes += cfg.batch as u64;
            hashrate_window += cfg.batch as u64;
            match found {
                Some(s) => break s,
                None => nonce_start = nonce_start.wrapping_add(cfg.batch as u64),
            }

            // Report hashrate every ~2s.
            let elapsed = mine_start.elapsed();
            if elapsed.as_secs() >= 2 {
                let mhs = hashrate_window as f64 / elapsed.as_secs_f64() / 1e6;
                log::info!("  [rate] {:.1} MH/s (block #{})", mhs, tmpl.block_number);
                mine_start = Instant::now();
                hashrate_window = 0;
            }
        };

        let seal_bytes = seal.to_seal_bytes();
        log::info!(
            "⛏️  mined nonce={:#x} mix=0x{}... result=0x{}...",
            seal.nonce,
            hex::encode(&seal.mix[..4]),
            hex::encode(&seal.result[..4])
        );

        // 3. Submit the seal.
        match rpc.submit_work(&pre_hash_hex, &seal_bytes) {
            Ok(true) => log::info!(
                "✅ SUBMITTED block #{} — accepted by node",
                tmpl.block_number
            ),
            Ok(false) => log::info!(
                "↩️  stale template (block #{}) — refetching",
                tmpl.block_number
            ),
            Err(e) => log::error!("[error] submitWork: {e}"),
        }

        // Session hashrate summary.
        let session_elapsed = session_start.elapsed();
        if session_elapsed.as_secs() >= 30 {
            let mhs = total_hashes as f64 / session_elapsed.as_secs_f64() / 1e6;
            log::info!(
                "  [session] {:.1} MH/s avg over {}s",
                mhs,
                session_elapsed.as_secs()
            );
            session_start = Instant::now();
            total_hashes = 0;
        }

        // Refetch immediately; the node has a new build after import. Check
        // the stop flag in small increments so cancellation is responsive.
        if wait_interruptible(&stop, 100) {
            break;
        }
    }
    log::info!("mining loop exited");
}

/// Sleep for `ms` ms, but return early (true) if `stop` becomes set.
fn wait_interruptible(stop: &AtomicBool, ms: u64) -> bool {
    let deadline = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < deadline {
        if stop.load(Ordering::SeqCst) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(
            10.min(
                deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis() as u64,
            )
            .max(1),
        ));
    }
    false
}

/// JSON-RPC client over HTTP (blocking) for the node's external-mining API.
struct BlockingMinerRpc {
    url: String,
    client: reqwest::blocking::Client,
    next_id: u64,
}

struct MiningTemplate {
    pre_hash: [u8; 32],
    difficulty: String,
    block_number: u32,
    best_hash: String,
}

impl BlockingMinerRpc {
    fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
            client: reqwest::blocking::Client::new(),
            next_id: 1,
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
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
            .map_err(|e| format!("rpc {method} transport: {e}"))?;
        let v: Value = resp
            .json()
            .map_err(|e| format!("rpc {method} decode: {e}"))?;
        if let Some(err) = v.get("error") {
            return Err(format!("rpc {method} error: {err}"));
        }
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }

    fn get_template(&mut self) -> Result<Option<MiningTemplate>, String> {
        let r = self.call("gpn_getTemplate", json!([]))?;
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
        let best_hash = r
            .get("best_hash")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let mut pre_hash = [0u8; 32];
        let bytes = hex::decode(pre_hex.trim_start_matches("0x"))
            .map_err(|e| format!("template pre_hash hex: {e}"))?;
        if bytes.len() != 32 {
            return Err(format!(
                "template pre_hash must be 32 bytes, got {}",
                bytes.len()
            ));
        }
        pre_hash.copy_from_slice(&bytes);

        Ok(Some(MiningTemplate {
            pre_hash,
            difficulty: difficulty.to_string(),
            block_number,
            best_hash,
        }))
    }

    fn submit_work(&mut self, pre_hash_hex: &str, seal: &[u8]) -> Result<bool, String> {
        let seal_hex = format!("0x{}", hex::encode(seal));
        let r = self.call("gpn_submitWork", json!([pre_hash_hex, seal_hex]))?;
        r.as_bool()
            .ok_or("submitWork result not a bool".to_string())
    }
}

/// Convert a big-endian hex difficulty string (U256 display) into a 32-byte
/// little-endian target array for the GPU kernel.
fn difficulty_to_le_target(difficulty: &str) -> [u8; 32] {
    let value = U256::from_str_radix(difficulty.trim_start_matches("0x"), 16)
        .expect("difficulty hex is a valid U256");
    let mut target = [0u8; 32];
    value.to_little_endian(&mut target);
    target
}
