//! External GPU mining engine — a *stoppable* wrapper that talks to the
//! node's external-mining RPC (`gpn_getTemplate`) and claims the block reward
//! via a signed `GPUPoW::submit_solution`.
//!
//! This is the production mining substrate for the agent's **auto-switch**
//! feature: the agent can start mining (spawn the loop), and later stop it to
//! free the machine for a compute job. Mining is just a startable/stoppable
//! task here, not the whole process.
//!
//! The loop runs on a dedicated OS thread (not a tokio task) so it can run the
//! CPU blake2 PoW search without blocking the async runtime. All network I/O
//! inside the thread uses `reqwest::blocking`, so the thread is self-contained
//! and safe to cancel.

use crate::gpu::ethash_miner::EthashGpuMiner;
use serde_json::{json, Value};
use sp_core::U256;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use subxt::dynamic::Value as SubxtValue;
use subxt::tx::Signer as _;

/// The runtime's current `pallet_gpu_pow::AlgorithmVersion` (see
/// `gpn-node/runtime/src/lib.rs`). A submitted solution must carry this value.
const ALGORITHM_VERSION: u32 = 1;

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
    /// Miner wallet secret URI (seed hex or dev URI). The block reward is paid
    /// to this account via a direct `submit_solution` (per-miner rewards).
    pub signer_uri: String,
}

impl MiningConfig {
    pub fn new(rpc_url: &str) -> Self {
        Self {
            rpc_url: rpc_url.to_string(),
            device_index: 0,
            batch: 1 << 20,
            poll_ms: 2000,
            signer_uri: "//Alice".to_string(),
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
    /// Start the mining loop on a background thread. Returns immediately;
    /// mining proceeds in the background.
    ///
    /// The reward-claim PoW (`GPUPoW::submit_solution`) is a blake2 hashcash
    /// (`blake2(nonce, mix, version) < target`), computed on the CPU, so no
    /// OpenCL DAG is built here. We still detect the GPU up front so the
    /// `gpn_external_miner` binary keeps its GPU diagnostics and fails fast on
    /// a GPU-less box.
    pub fn start(cfg: MiningConfig) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));

        let miner =
            EthashGpuMiner::new(cfg.device_index).map_err(|e| format!("open GPU miner: {e}"))?;
        let device_name = miner.device_name();
        drop(miner);

        let stop_clone = stop.clone();
        let thread = std::thread::Builder::new()
            .name("gpn-miner".into())
            .spawn(move || {
                mining_loop(cfg, stop_clone);
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
fn mining_loop(cfg: MiningConfig, stop: Arc<AtomicBool>) {
    let mut rpc = BlockingMinerRpc::new(&cfg.rpc_url);

    let mut last_block: Option<u32> = None;
    let mut nonce: u64 = 0;
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

        let difficulty_u256 = U256::from_str_radix(tmpl.difficulty.trim_start_matches("0x"), 16)
            .expect("difficulty hex is a valid U256");

        // 2. Search for a nonce whose `blake2(nonce, mix, version)` is below
        // the difficulty target. `pallet_gpu_pow::submit_solution` verifies
        // that blake2 hash (NOT an ethash result), so this is a CPU blake2
        // hashcash search rather than the GPU ethash used for block authoring.
        let seal = loop {
            if stop.load(Ordering::SeqCst) {
                log::info!("mining loop cancelled mid-search — stopping");
                return;
            }
            match search_solution(&difficulty_u256, nonce, cfg.batch, &stop) {
                Some(s) => {
                    nonce = s.nonce.wrapping_add(1);
                    break s;
                }
                None => nonce = nonce.wrapping_add(cfg.batch as u64),
            }
            total_hashes += cfg.batch as u64;
            hashrate_window += cfg.batch as u64;

            // Report hashrate every ~2s.
            let elapsed = mine_start.elapsed();
            if elapsed.as_secs() >= 2 {
                let mhs = hashrate_window as f64 / elapsed.as_secs_f64() / 1e6;
                log::info!("  [rate] {:.1} MH/s (block #{})", mhs, tmpl.block_number);
                mine_start = Instant::now();
                hashrate_window = 0;
            }
        };

        log::info!(
            "⛏️  mined nonce={:#x} solution_hash=0x{}...",
            seal.nonce,
            hex::encode(&seal.result[..4])
        );

        // 3. Submit the solution directly (signed) so the reward goes to the miner's wallet.
        match submit_solution_direct(&cfg.rpc_url, &cfg.signer_uri, &seal, &tmpl.difficulty) {
            Ok(true) => log::info!(
                "✅ SUBMITTED block #{} — reward paid to miner wallet",
                tmpl.block_number
            ),
            Ok(false) => log::info!(
                "↩️  stale (block #{}) — refetching",
                tmpl.block_number
            ),
            Err(e) => log::error!("[error] submit_solution: {e}"),
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

/// Search `batch` nonces starting at `start_nonce` (with a fixed zero
/// `mix_hash`) for the first whose `blake2(nonce, mix, version)` is below
/// `difficulty`. Returns the seal (with `result` = the blake2 solution hash)
/// or `None` if no nonce in this window qualifies.
fn search_solution(
    difficulty: &U256,
    start_nonce: u64,
    batch: usize,
    stop: &AtomicBool,
) -> Option<crate::gpu::ethash_miner::MinedSeal> {
    let mix = [0u8; 32];
    let mut nonce = start_nonce;
    for _ in 0..batch {
        if stop.load(Ordering::SeqCst) {
            return None;
        }
        if meets_target(nonce, &mix, ALGORITHM_VERSION, difficulty) {
            let result = solution_hash(nonce, &mix, ALGORITHM_VERSION);
            return Some(crate::gpu::ethash_miner::MinedSeal { nonce, mix, result });
        }
        nonce = nonce.wrapping_add(1);
    }
    None
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

/// Compute the `solution_hash` the pallet expects for a seal.
///
/// `pallet_gpu_pow::compute_solution_hash` hashes the SCALE encoding of
/// `(nonce, mix_hash, algorithm_version)` with blake2-256 — **not** the ethash
/// final "result" hash. The SCALE encoding of that 3-tuple is the 44 raw bytes
/// `nonce_le(8) ++ mix_hash(32) ++ version_le(4)` (no length prefixes).
fn solution_hash(nonce: u64, mix: &[u8; 32], algorithm_version: u32) -> [u8; 32] {
    let mut work = [0u8; 44];
    work[0..8].copy_from_slice(&nonce.to_le_bytes());
    work[8..40].copy_from_slice(mix);
    work[40..44].copy_from_slice(&algorithm_version.to_le_bytes());
    sp_core::hashing::blake2_256(&work)
}

/// Whether a seal's `solution_hash` meets the difficulty *target*.
///
/// `pallet_gpu_pow::meets_difficulty` treats `difficulty` as a target and
/// requires `blake2(nonce, mix, version) < difficulty` (higher = easier).
fn meets_target(nonce: u64, mix: &[u8; 32], algorithm_version: u32, difficulty: &U256) -> bool {
    let h = solution_hash(nonce, mix, algorithm_version);
    U256::from_little_endian(&h) < *difficulty
}

/// Submit a mined seal directly to `GPUPoW::submit_solution`, signed with the
/// miner's own key, so the block reward is paid to the miner's wallet (rather
/// than the node's configured `BlockRewardBeneficiary`).
fn submit_solution_direct(
    endpoint: &str,
    signer_uri: &str,
    seal: &crate::gpu::ethash_miner::MinedSeal,
    difficulty: &str,
) -> Result<bool, String> {
    let difficulty_u256 = U256::from_str_radix(difficulty.trim_start_matches("0x"), 16)
        .map_err(|e| format!("parse difficulty: {e}"))?;
    // The pallet's `U256` is `[u64; 4]` (little-endian limbs), so encode the
    // difficulty as an unnamed composite of four u64s — NOT `Primitive::U256`
    // (a 32-byte array, which fails to encode against `[u64; 4]`).
    let limbs = difficulty_u256.0;

    let solution_hash = solution_hash(seal.nonce, &seal.mix, ALGORITHM_VERSION);

    // subxt's `OnlineClient` needs a ws(s) endpoint even though the template
    // fetch (`get_template`) uses plain HTTP JSON-RPC. Rewrite the scheme here.
    let ws_endpoint = endpoint
        .replace("http://", "ws://")
        .replace("https://", "wss://");

    // subxt's `OnlineClient` (jsonrpsee ws) requires a Tokio reactor, and this
    // runs on the dedicated mining thread (not the async runtime), so drive the
    // whole async submission block with a fresh current-thread Tokio runtime.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    rt.block_on(async {
        let client = crate::chain::signer::connect(&ws_endpoint)
            .await
            .map_err(|e| format!("connect: {e}"))?;
        let signer = crate::chain::signer::Sr25519Signer::from_uri(signer_uri)
            .map_err(|e| format!("signer: {e}"))?;

        let solution = SubxtValue::unnamed_composite(vec![
            SubxtValue::u128(seal.nonce as u128),
            SubxtValue::from_bytes(&seal.mix),
            SubxtValue::from_bytes(&solution_hash),
            SubxtValue::u128(ALGORITHM_VERSION as u128),
        ]);
        let difficulty_val = SubxtValue::unnamed_composite(vec![
            SubxtValue::u128(limbs[0] as u128),
            SubxtValue::u128(limbs[1] as u128),
            SubxtValue::u128(limbs[2] as u128),
            SubxtValue::u128(limbs[3] as u128),
        ]);
        let call =
            subxt::dynamic::tx("GPUPoW", "submit_solution", vec![solution, difficulty_val]);

        let nonce =
            crate::chain::signer::current_nonce(&client, endpoint, &signer.account_id()).await;
        crate::chain::signer::submit_signed(&client, &call, &signer, nonce)
            .await
            .map_err(|e| format!("submit_solution: {e}"))?;
        Ok(true)
    })
}
