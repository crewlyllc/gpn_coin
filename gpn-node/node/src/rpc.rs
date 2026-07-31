//! External miner RPC for the GPN node.
//!
//! This is the **production mining architecture** substrate: it lets a miner
//! process (the `gpn-agent`, or any standalone GPU miner) fetch a block
//! template over RPC, mine on its own GPU, and submit the finished seal back
//! over RPC — without running a full node. Separating the node from the miner
//! is a prerequisite for the headline "GPU auto-switch" feature: the same GPU
//! can spend one period serving AI compute and the next mining, while the
//! lightweight node client stays connected.
//!
//! Two methods:
//! - `gpn_getTemplate` — returns the current mining build (pre-hash, target
//!   difficulty, best hash, block number) for an external miner to work on.
//! - `gpn_submitWork`   — submits a finished Ethash seal (nonce + mix hash)
//!   for the current template. The seal is re-verified by the consensus
//!   engine before import.

use jsonrpsee::core::RpcResult;
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::types::ErrorObject;
use serde::{Deserialize, Serialize};
use sp_blockchain::HeaderBackend;
use std::sync::{Arc, Mutex};

use crate::service::{FullClient, MiningWorker};

/// A mining template returned to an external miner.
///
/// All hashes/difficulty are hex strings (0x-prefixed, big-endian) so the
/// wire format is trivial for any client (Rust subxt, Python, etc.) to parse.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MiningTemplate {
    /// Best known block hash the template is built on top of.
    pub best_hash: String,
    /// The block pre-hash the miner must solve against (the Ethash header).
    pub pre_hash: String,
    /// The PoW target. A valid solution has a hash *below* this target
    /// (the `sc_consensus_pow` / `pallet-gpu-pow` convention).
    pub difficulty: String,
    /// Block number of the best chain tip.
    pub block_number: u32,
}

/// RPC surface for external miners.
#[rpc(client, server)]
pub trait GpnMinerApi {
    /// Fetch the current mining template, or `None` if the node is not yet
    /// producing builds (still syncing / worker not started).
    #[method(name = "gpn_getTemplate")]
    async fn get_template(&self) -> RpcResult<Option<MiningTemplate>>;

    /// Submit a finished seal (hex: 8-byte LE nonce ++ 32-byte mix hash) for
    /// the current template. Returns `true` if the block was imported.
    ///
    /// `pre_hash` must match the current template's pre-hash; a stale or
    /// mismatched submission returns `false` (the block build it targeted is
    /// already gone).
    #[method(name = "gpn_submitWork")]
    async fn submit_work(&self, pre_hash: String, seal: String) -> RpcResult<bool>;
}

/// Implementation backed by the shared `MiningHandle` slot.
///
/// The worker is created inside `service::new_full` (authority branch) and
/// parked in a shared `Arc<Mutex<Option<_>>>` slot so the RPC can reach it
/// lazily at call time. Cloning the handle shares the same build/import state.
pub struct GpnMinerRpc {
    worker: Arc<Mutex<Option<MiningWorker>>>,
    client: Arc<FullClient>,
}

impl GpnMinerRpc {
    pub fn new(worker: Arc<Mutex<Option<MiningWorker>>>, client: Arc<FullClient>) -> Self {
        Self { worker, client }
    }

    /// Clone the worker handle out of the slot, if present.
    fn worker(&self) -> RpcResult<MiningWorker> {
        self.worker
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| ErrorObject::owned(-32000, "mining worker not available", None::<()>))
    }

    /// Decode a 0x-prefixed hex string into raw bytes.
    fn decode_hex(s: &str) -> RpcResult<Vec<u8>> {
        let s = s.strip_prefix("0x").unwrap_or(s);
        hex::decode(s)
            .map_err(|e| ErrorObject::owned(-32000, format!("invalid hex: {e}"), None::<()>))
    }
}

#[jsonrpsee::core::async_trait]
impl GpnMinerApiServer for GpnMinerRpc {
    async fn get_template(&self) -> RpcResult<Option<MiningTemplate>> {
        let worker = self.worker()?;
        let metadata = match worker.metadata() {
            Some(m) => m,
            None => return Ok(None),
        };

        let block_number = self
            .client
            .header(metadata.best_hash)
            .ok()
            .flatten()
            .map(|h| h.number)
            .unwrap_or(0);

        Ok(Some(MiningTemplate {
            best_hash: format!("{:#x}", metadata.best_hash),
            pre_hash: format!("{:#x}", metadata.pre_hash),
            difficulty: format!("{:#x}", metadata.difficulty),
            block_number,
        }))
    }

    async fn submit_work(&self, pre_hash: String, seal: String) -> RpcResult<bool> {
        // Clone the worker handle out of the slot *before* any await so we
        // never hold the slot lock across a suspend point.
        let worker = self.worker()?;
        let seal = Self::decode_hex(&seal)?;

        // Reject submissions that target a stale template. The consensus
        // engine would reject them anyway, but failing fast gives the miner
        // an immediate "refetch the template" signal.
        if let Some(metadata) = worker.metadata() {
            if format!("{:#x}", metadata.pre_hash) != pre_hash {
                return Ok(false);
            }
        } else {
            return Ok(false);
        }

        // `MiningHandle::submit` holds a non-`Send` mutex guard across an
        // await (block import), so it cannot be polled on the async runtime
        // directly. Run it to completion on a blocking thread instead.
        let ok = tokio::task::spawn_blocking(move || {
            futures::executor::block_on(worker.submit(seal))
        })
        .await
        .map_err(|e| ErrorObject::owned(-32000, format!("submit task failed: {e}"), None::<()>))?;

        Ok(ok)
    }
}

/// Convenience: build an `RpcModule` with the GPN miner RPC registered.
pub fn create_full_rpc(
    worker: Arc<Mutex<Option<MiningWorker>>>,
    client: Arc<FullClient>,
) -> jsonrpsee::RpcModule<()> {
    let mut module = jsonrpsee::RpcModule::new(());
    let rpc = GpnMinerRpc::new(worker, client);
    module
        .merge(rpc.into_rpc())
        .expect("gpn miner rpc methods are unique");
    module
}
