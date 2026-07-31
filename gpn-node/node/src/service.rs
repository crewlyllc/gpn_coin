//! Service and ServiceFactory implementation for the GPN PoW node.

use codec::Decode;
use futures::FutureExt;
use gpn_runtime::{self, Block, RuntimeApi};
use sc_basic_authorship::ProposerFactory;
use sc_client_api::BlockBackend;
use sc_client_api::Backend;
use sc_client_api::StorageProvider;
use sc_consensus::{BlockImport, LongestChain};
use sc_consensus_pow::{PowAlgorithm, PowBlockImport, PowImportQueue, start_mining_worker};
use sc_network_sync::SyncingService;
use sc_service::{error::Error as ServiceError, Configuration, TaskManager};
use sc_telemetry::{Telemetry, TelemetryWorker};
use sc_transaction_pool_api::OffchainTransactionPoolFactory;
use sp_consensus_pow::{Seal, POW_ENGINE_ID};
use sp_core::storage::StorageKey;
use sp_core::U256;
use sp_io::hashing::twox_128;
use sp_runtime::generic::BlockId;
use sp_runtime::traits::Block as BlockT;
use std::{sync::Arc, sync::Mutex, sync::OnceLock, time::Duration};

use crate::gpu_miner::EthashGpuMiner;

pub(crate) type FullClient = sc_service::TFullClient<
    Block,
    RuntimeApi,
    sc_executor::WasmExecutor<sp_io::SubstrateHostFunctions>,
>;
type FullBackend = sc_service::TFullBackend<Block>;
type FullSelectChain = LongestChain<FullBackend, Block>;

/// Dev/testnet PoW *target* (hash < target = valid, so HIGHER = EASIER).
///
/// This is the convention `sc_consensus_pow` uses and the one `pallet-gpu-pow`
/// follows internally. NOTE: it is the inverse of Ethereum's "difficulty"
/// (where higher = harder). Here 2^240 gives P ~ 1.5e-5 per hash — a CPU
/// miner using the light cache finds a block in tens of seconds on the dev
/// chain.
///
/// This is now a **fallback only**: the live difficulty is read from
/// `pallet-gpu-pow`'s stored `Difficulty` target (genesis = 2^244), which the
/// chain self-tunes every block. `initial_target()` is used only if that
/// storage read fails.
fn initial_target() -> U256 {
    // 2^244: byte[1] = 0x10, rest 0. Slightly easier than 2^240 so the
    // light-cache CPU miner produces blocks at a reasonable dev cadence.
    let mut buf = [0u8; 32];
    buf[1] = 0x10;
    U256::from_big_endian(&buf)
}

/// Storage prefix for `pallet-gpu-pow` as named in `construct_runtime!`
/// (`GPUPoW: pallet_gpu_pow` in the runtime).
const GPU_POW_PREFIX: &[u8] = b"GPUPoW";
/// Storage item name of the pallet's difficulty target.
const DIFFICULTY_ITEM: &[u8] = b"Difficulty";

/// Convert the pallet's `[u64; 4]` little-endian-limbed target into a
/// `sp_core::U256` (same numeric value).
fn limbs_to_u256(limbs: [u64; 4]) -> U256 {
    let mut bytes = [0u8; 32];
    for (i, limb) in limbs.iter().enumerate() {
        bytes[i * 8..i * 8 + 8].copy_from_slice(&limb.to_le_bytes());
    }
    U256::from_little_endian(&bytes)
}

/// The twox128-prefixed storage key for `pallet-gpu-pow::Difficulty`.
fn difficulty_storage_key() -> StorageKey {
    let mut key = Vec::with_capacity(32);
    key.extend_from_slice(&twox_128(GPU_POW_PREFIX));
    key.extend_from_slice(&twox_128(DIFFICULTY_ITEM));
    StorageKey(key)
}

/// Dev chain uses Ethash epoch 0 (smallest cache, 16 MB) so light-cache CPU
/// mining is feasible without loading the full ~1 GB dataset.
const DEV_EPOCH: usize = 0;

/// The Ethash seal layout: 8-byte little-endian nonce ++ 32-byte mix hash.
const SEAL_NONCE_LEN: usize = 8;
const SEAL_MIX_LEN: usize = 32;
const SEAL_LEN: usize = SEAL_NONCE_LEN + SEAL_MIX_LEN;

/// Lazily-built Ethash light cache for the dev epoch. Verification on every
/// block import and mining both share this cache.
fn ethash_cache() -> &'static [u8] {
    static CACHE: OnceLock<Vec<u8>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let seed = ethash::get_seedhash(DEV_EPOCH);
        let cache_size = ethash::get_cache_size(DEV_EPOCH);
        let mut cache = vec![0u8; cache_size];
        ethash::make_cache(&mut cache, seed);
        cache
    })
}

/// Full dataset size for the dev epoch (used only to size hashimoto lookups;
/// light mode computes dataset items from the cache on the fly).
fn ethash_full_size() -> usize {
    ethash::get_full_size(DEV_EPOCH)
}

/// A real Ethash PoW algorithm for GPN.
///
/// Blocks are sealed with a genuine Ethash solution (nonce + mix_hash) and
/// verified via the standard `hashimoto_light`. The same algorithm a GPU
/// miner targets with the full dataset — GPU-compatible by construction.
///
/// Difficulty is **self-tuning**: `difficulty()` reads the `Difficulty`
/// target stored by `pallet-gpu-pow` at the parent block (which the pallet
/// adjusts every block toward the target block time), instead of returning a
/// fixed placeholder.
#[derive(Clone)]
pub struct GPNPowAlgorithm {
    client: Arc<FullClient>,
}

/// Verify a seal: parse nonce + mix_hash, recompute Ethash over the block
/// pre-hash, and require the computed mix hash to match AND the final hash to
/// be below the target.
fn verify_seal(pre_hash: &<Block as BlockT>::Hash, seal: &Seal, target: U256) -> bool {
    if seal.len() != SEAL_LEN {
        return false;
    }
    let nonce = u64::from_le_bytes(seal[..SEAL_NONCE_LEN].try_into().unwrap());
    let mut mix = [0u8; SEAL_MIX_LEN];
    mix.copy_from_slice(&seal[SEAL_NONCE_LEN..SEAL_LEN]);

    let header_hash = ethereum_types::H256::from_slice(pre_hash.as_ref());
    let (computed_mix, result) = ethash::hashimoto_light(
        header_hash,
        ethereum_types::H64::from_low_u64_be(nonce),
        ethash_full_size(),
        ethash_cache(),
    );

    if computed_mix.as_bytes() != mix {
        return false;
    }
    U256::from_little_endian(result.as_bytes()) < target
}

impl PowAlgorithm<Block> for GPNPowAlgorithm {
    type Difficulty = U256;

    fn difficulty(&self, parent: <Block as BlockT>::Hash) -> Result<Self::Difficulty, sc_consensus_pow::Error<Block>> {
        // Read the pallet's stored difficulty target at the parent block. The
        // pallet self-tunes it each block toward the target block time.
        let data = self
            .client
            .storage(parent, &difficulty_storage_key())
            .ok()
            .flatten();
        if let Some(data) = data {
            if let Ok(limbs) = <[u64; 4]>::decode(&mut &data.0[..]) {
                return Ok(limbs_to_u256(limbs));
            }
        }
        // Fallback (shouldn't happen post-genesis): a dev-friendly target.
        Ok(initial_target())
    }

    fn verify(
        &self,
        _parent: &BlockId<Block>,
        pre_hash: &<Block as BlockT>::Hash,
        _pre_digest: Option<&[u8]>,
        seal: &Seal,
        difficulty: Self::Difficulty,
    ) -> Result<bool, sc_consensus_pow::Error<Block>> {
        Ok(verify_seal(pre_hash, seal, difficulty))
    }
}

/// The inherent data providers for this chain: timestamp only.
///
/// Returns a closure (rather than a named fn) so the future's error type is
/// inferred from the caller's `CreateInherentDataProviders` bound.
fn create_inherent_data_providers() -> impl Fn(<Block as BlockT>::Hash, ()) -> futures::future::BoxFuture<'static, Result<sp_timestamp::InherentDataProvider, Box<dyn std::error::Error + Send + Sync>>> {
    move |_parent: <Block as BlockT>::Hash, _data: ()| {
        Box::pin(async move {
            Ok(sp_timestamp::InherentDataProvider::from_system_time())
        })
    }
}

pub type Service = sc_service::PartialComponents<
    FullClient,
    FullBackend,
    FullSelectChain,
    PowImportQueue<Block>,
    sc_transaction_pool::TransactionPoolHandle<Block, FullClient>,
    (
        GPNPowAlgorithm,
        Option<Telemetry>,
        // Shared slot the external-miner RPC reads the worker from. Populated
        // in `new_full` once the authority mining worker starts.
        Arc<Mutex<Option<MiningWorker>>>,
    ),
>;

pub fn new_partial(config: &Configuration) -> Result<Service, ServiceError> {
    let telemetry = config
        .telemetry_endpoints
        .clone()
        .filter(|x| !x.is_empty())
        .map(|endpoints| -> Result<_, sc_telemetry::Error> {
            let worker = TelemetryWorker::new(16)?;
            let telemetry = worker.handle().new_telemetry(endpoints);
            Ok((worker, telemetry))
        })
        .transpose()?;

    let executor = sc_service::new_wasm_executor::<sp_io::SubstrateHostFunctions>(&config.executor);

    let (client, backend, keystore_container, task_manager) =
        sc_service::new_full_parts::<Block, RuntimeApi, _>(
            config,
            telemetry.as_ref().map(|(_, telemetry)| telemetry.handle()),
            executor,
            Vec::new(),
        )?;
    let client = Arc::new(client);

    let telemetry = telemetry.map(|(worker, telemetry)| {
        task_manager.spawn_handle().spawn("telemetry", None, worker.run());
        telemetry
    });

    let select_chain = LongestChain::new(backend.clone());

    let transaction_pool = Arc::from(
        sc_transaction_pool::Builder::new(
            task_manager.spawn_essential_handle(),
            client.clone(),
            config.role.is_authority().into(),
        )
        .with_options(config.transaction_pool.clone())
        .with_prometheus(config.prometheus_registry())
        .build(),
    );

    let algorithm = GPNPowAlgorithm { client: client.clone() };

    // Shared slot that the authority mining worker is parked into later, so
    // the external-miner RPC can reach it lazily at call time.
    let miner_worker_slot: Arc<Mutex<Option<MiningWorker>>> = Arc::new(Mutex::new(None));

    let block_import = PowBlockImport::new(
        client.clone(),
        client.clone(),
        algorithm.clone(),
        0u32.into(),
        select_chain.clone(),
        create_inherent_data_providers(),
    );

    let import_queue = sc_consensus_pow::import_queue(
        Box::new(block_import),
        None,
        algorithm.clone(),
        &task_manager.spawn_essential_handle(),
        config.prometheus_registry(),
    )?;

    Ok(sc_service::PartialComponents {
        client,
        backend,
        task_manager,
        import_queue,
        keystore_container,
        select_chain,
        transaction_pool,
        other: (algorithm, telemetry, miner_worker_slot),
    })
}

/// Builds a new service for a full client.
pub fn new_full<
    N: sc_network::NetworkBackend<Block, <Block as sp_runtime::traits::Block>::Hash>,
>(
    config: Configuration,
) -> Result<TaskManager, ServiceError> {
    let sc_service::PartialComponents {
        client,
        backend,
        mut task_manager,
        import_queue,
        keystore_container,
        select_chain,
        transaction_pool,
        other: (algorithm, mut telemetry, miner_worker_slot),
    } = new_partial(&config)?;

    let net_config = sc_network::config::FullNetworkConfiguration::<
        Block,
        <Block as sp_runtime::traits::Block>::Hash,
        N,
    >::new(&config.network, config.prometheus_registry().cloned());
    let metrics = N::register_notification_metrics(config.prometheus_registry());

    let (network, system_rpc_tx, tx_handler_controller, sync_service) =
        sc_service::build_network(sc_service::BuildNetworkParams {
            config: &config,
            net_config,
            client: client.clone(),
            transaction_pool: transaction_pool.clone(),
            spawn_handle: task_manager.spawn_handle(),
            spawn_essential_handle: task_manager.spawn_essential_handle(),
            import_queue,
            block_announce_validator_builder: None,
            warp_sync_config: None,
            block_relay: None,
            metrics,
        })?;

    if config.offchain_worker.enabled {
        let offchain_workers =
            sc_offchain::OffchainWorkers::new(sc_offchain::OffchainWorkerOptions {
                runtime_api_provider: client.clone(),
                is_validator: config.role.is_authority(),
                keystore: Some(keystore_container.keystore()),
                offchain_db: backend.offchain_storage(),
                transaction_pool: Some(OffchainTransactionPoolFactory::new(
                    transaction_pool.clone(),
                )),
                network_provider: Arc::new(network.clone()),
                enable_http_requests: true,
                custom_extensions: |_| vec![],
            })?;
        task_manager.spawn_handle().spawn(
            "offchain-workers-runner",
            "offchain-worker",
            offchain_workers.run(client.clone(), task_manager.spawn_handle()).boxed(),
        );
    }

    let role = config.role;
    let force_authoring = config.force_authoring;
    let prometheus_registry = config.prometheus_registry().cloned();

    let rpc_extensions_builder = {
        let client = client.clone();
        let miner_worker_slot = miner_worker_slot.clone();
        Box::new(move |_| {
            Ok(crate::rpc::create_full_rpc(miner_worker_slot.clone(), client.clone()))
        })
    };

    let _rpc_handlers = sc_service::spawn_tasks(sc_service::SpawnTasksParams {
        network: Arc::new(network.clone()),
        client: client.clone(),
        keystore: keystore_container.keystore(),
        task_manager: &mut task_manager,
        transaction_pool: transaction_pool.clone(),
        rpc_builder: rpc_extensions_builder,
        backend,
        system_rpc_tx,
        tx_handler_controller,
        sync_service: sync_service.clone(),
        config,
        telemetry: telemetry.as_mut(),
        tracing_execute_block: None,
    })?;

    if role.is_authority() {
        let proposer_factory = ProposerFactory::new(
            task_manager.spawn_handle(),
            client.clone(),
            transaction_pool.clone(),
            prometheus_registry.as_ref(),
            telemetry.as_ref().map(|x| x.handle()),
        );

        // Reconstruct the PoW block import for the miner (not Clone, so rebuilt here).
        let block_import = PowBlockImport::new(
            client.clone(),
            client.clone(),
            algorithm.clone(),
            0u32.into(),
            select_chain.clone(),
            create_inherent_data_providers(),
        );

        let (worker, task) = start_mining_worker(
            Box::new(block_import),
            client.clone(),
            select_chain,
            algorithm,
            proposer_factory,
            sync_service.clone(),
            sync_service,
            Some(POW_ENGINE_ID.to_vec()),
            create_inherent_data_providers(),
            Duration::from_secs(10),
            Duration::from_secs(5),
        );

        task_manager.spawn_essential_handle().spawn("pow-mining-worker", None, task);

        // Park a clone of the worker in the shared slot so external miners can
        // fetch templates and submit seals over RPC.
        *miner_worker_slot.lock().unwrap() = Some(worker.clone());

        if force_authoring {
            spawn_miner(worker);
        }
    }

    Ok(task_manager)
}

pub(crate) type MiningWorker = sc_consensus_pow::MiningHandle<Block, GPNPowAlgorithm, Arc<SyncingService<Block>>>;

/// How many nonces the GPU searches per `worker.metadata()` poll. Large enough
/// to find a dev-difficulty solution in one shot, but bounded so we regularly
/// re-check for a newer block build.
const GPU_BATCH: usize = 1 << 20;

/// Spawn a dedicated miner thread: poll `worker.metadata()`, run real Ethash
/// mining to find a valid nonce + mix_hash, then submit the seal. Runs on its
/// own OS thread because the submit future holds a non-`Send` mutex guard
/// across an await.
///
/// Tries the **GPU OpenCL miner first** (full dataset on the GPU). If no
/// OpenCL GPU is available, falls back to the light-cache CPU miner so the
/// node still produces blocks on machines without a GPU.
fn spawn_miner(worker: MiningWorker) {
    match EthashGpuMiner::new(0) {
        Ok(_) => spawn_gpu_miner(worker),
        Err(e) => {
            log::warn!("GPU miner unavailable ({e}); falling back to CPU light-cache miner");
            spawn_cpu_miner(worker);
        }
    }
}

/// GPU miner: build the full Ethash dataset on the GPU once, then loop over
/// `worker.metadata()` and search nonce batches in parallel on the GPU.
/// The node's `verify_seal` recomputes with `hashimoto_light`, so the GPU's
/// full-DAG solution is accepted as long as mix + result match (they do — the
/// miner self-validates against the `ethash` crate reference).
fn spawn_gpu_miner(worker: MiningWorker) {
    std::thread::Builder::new()
        .name("gpn-gpu-miner".into())
        .spawn(move || {
            let miner = match EthashGpuMiner::new(0) {
                Ok(m) => m,
                Err(e) => {
                    log::error!("GPU miner init failed: {e}");
                    return;
                }
            };
            log::info!("GPU Ethash miner online: {}", miner.device_name());

            // Build the epoch-0 light cache + full DAG on the GPU.
            let seed = ethash::get_seedhash(DEV_EPOCH);
            let cache_size = ethash::get_cache_size(DEV_EPOCH);
            let full_size = ethash::get_full_size(DEV_EPOCH);
            let mut cache = vec![0u8; cache_size];
            ethash::make_cache(&mut cache, seed);
            let dag = match miner.build_dag(&cache, full_size) {
                Ok(d) => d,
                Err(e) => {
                    log::error!("GPU DAG build failed: {e}");
                    return;
                }
            };

            let mut last_pre_hash: Option<<Block as BlockT>::Hash> = None;
            let mut nonce: u64 = 0;
            loop {
                if let Some(metadata) = worker.metadata() {
                    let pre_hash = metadata.pre_hash;
                    let target = metadata.difficulty;

                    // On a new block build, restart the nonce window.
                    if Some(pre_hash) != last_pre_hash {
                        nonce = 0;
                        last_pre_hash = Some(pre_hash);
                    }

                    let header: [u8; 32] = pre_hash.as_ref().try_into().expect("hash is 32 bytes");
                    // The pallet stores a *target* (hash < target = valid).
                    // GPU kernel compares little-endian, matching U256::to_little_endian.
                    let target_le: [u8; 32] = target.to_little_endian();

                    match miner.mine_batch(&dag, &header, &target_le, nonce, GPU_BATCH) {
                        Ok(Some(seal)) => {
                            let seal_bytes = seal.to_seal_bytes();
                            let _ = futures::executor::block_on(worker.submit(seal_bytes));
                            nonce = nonce.wrapping_add(GPU_BATCH as u64);
                        }
                        Ok(None) => {
                            // No solution in this window — advance and retry.
                            nonce = nonce.wrapping_add(GPU_BATCH as u64);
                        }
                        Err(e) => {
                            log::error!("GPU mine error: {e}");
                            std::thread::sleep(Duration::from_millis(100));
                        }
                    }
                } else {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        })
        .expect("failed to spawn GPU miner thread");
}

/// Light-cache CPU miner fallback (no GPU available).
fn spawn_cpu_miner(worker: MiningWorker) {
    std::thread::Builder::new()
        .name("gpn-cpu-miner".into())
        .spawn(move || {
            let cache = ethash_cache();
            let full_size = ethash_full_size();
            let mut nonce: u64 = 0;
            loop {
                if let Some(metadata) = worker.metadata() {
                    let pre_hash = metadata.pre_hash;
                    let target = metadata.difficulty;
                    let header_hash = ethereum_types::H256::from_slice(pre_hash.as_ref());
                    // Search a bounded window so we re-check for a new build.
                    for _ in 0..200_000 {
                        let (mix, result) = ethash::hashimoto_light(
                            header_hash,
                            ethereum_types::H64::from_low_u64_be(nonce),
                            full_size,
                            cache,
                        );
                        if U256::from_little_endian(result.as_bytes()) < target {
                            let mut seal = Vec::with_capacity(SEAL_LEN);
                            seal.extend_from_slice(&nonce.to_le_bytes());
                            seal.extend_from_slice(mix.as_bytes());
                            let _ = futures::executor::block_on(worker.submit(seal));
                            nonce = nonce.wrapping_add(1);
                            break;
                        }
                        nonce = nonce.wrapping_add(1);
                    }
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        })
        .expect("failed to spawn CPU miner thread");
}

#[cfg(test)]
mod seal_tests {
    use super::*;

    /// Build a genuine Ethash seal (nonce + mix_hash) below the given target
    /// using the light cache, exactly as `verify_seal` recomputes it.
    fn make_seal(pre_hash: &[u8; 32], target: U256) -> Vec<u8> {
        let header_hash = ethereum_types::H256::from_slice(pre_hash);
        let cache = ethash_cache();
        let full_size = ethash_full_size();
        let mut nonce: u64 = 0;
        loop {
            let (mix, result) = ethash::hashimoto_light(
                header_hash,
                ethereum_types::H64::from_low_u64_be(nonce),
                full_size,
                cache,
            );
            if U256::from_little_endian(result.as_bytes()) < target {
                let mut seal = Vec::with_capacity(SEAL_LEN);
                seal.extend_from_slice(&nonce.to_le_bytes());
                seal.extend_from_slice(mix.as_bytes());
                return seal;
            }
            nonce = nonce.wrapping_add(1);
        }
    }

    /// A valid, node-verifiable seal must pass `verify_seal`.
    #[test]
    fn valid_seal_passes_verify() {
        let pre_hash = [0xabu8; 32];
        // Very easy target (2^255) so a nonce is found quickly.
        let target = U256::from_big_endian(&{
            let mut b = [0u8; 32];
            b[0] = 0x7f;
            b
        });
        let seal = make_seal(&pre_hash, target);
        assert_eq!(seal.len(), SEAL_LEN, "seal must be exactly 40 bytes");
        assert!(verify_seal(&pre_hash.into(), &seal, target), "valid seal rejected");
    }

    /// A seal with the wrong mix_hash must be rejected even if the nonce is
    /// otherwise valid.
    #[test]
    fn wrong_mix_rejected() {
        let pre_hash = [0xabu8; 32];
        let target = U256::from_big_endian(&{
            let mut b = [0u8; 32];
            b[0] = 0x7f;
            b
        });
        let mut seal = make_seal(&pre_hash, target);
        let last = seal.len() - 1;
        seal[last] ^= 0xff; // corrupt the mix hash
        assert!(!verify_seal(&pre_hash.into(), &seal, target), "corrupt mix accepted");
    }

    /// Seals of the wrong length must always be rejected (no panic on short
    /// or long input).
    #[test]
    fn wrong_length_rejected() {
        let pre_hash = [0xabu8; 32];
        let target = U256::from_big_endian(&{
            let mut b = [0u8; 32];
            b[0] = 0x7f;
            b
        });
        let good = make_seal(&pre_hash, target);

        // Too short (39 bytes).
        let short: Vec<u8> = good[..SEAL_LEN - 1].to_vec();
        assert!(!verify_seal(&pre_hash.into(), &short, target), "short seal accepted");

        // Too long (41 bytes).
        let mut long = good.clone();
        long.push(0u8);
        assert!(!verify_seal(&pre_hash.into(), &long, target), "long seal accepted");
    }

    /// A genuine seal must be rejected when the target is below the produced
    /// hash (i.e. the block doesn't meet the required difficulty).
    #[test]
    fn above_target_rejected() {
        let pre_hash = [0xabu8; 32];
        // Start with an easy target to find a seal, then verify against a
        // much harder target (2^8) that the found hash will never satisfy.
        let easy = U256::from_big_endian(&{
            let mut b = [0u8; 32];
            b[0] = 0x7f;
            b
        });
        let seal = make_seal(&pre_hash, easy);
        let hard = U256::from_big_endian(&{
            let mut b = [0u8; 32];
            b[31] = 0xff; // tiny target 2^8
            b
        });
        assert!(!verify_seal(&pre_hash.into(), &seal, hard), "too-hard seal accepted");
    }

    /// A different block header (pre-hash) must invalidate an otherwise valid
    /// seal — seals are bound to a specific block.
    #[test]
    fn wrong_header_rejected() {
        let pre_hash = [0xabu8; 32];
        let other_pre_hash = [0xcd_u8; 32];
        let target = U256::from_big_endian(&{
            let mut b = [0u8; 32];
            b[0] = 0x7f;
            b
        });
        let seal = make_seal(&pre_hash, target);
        assert!(
            !verify_seal(&other_pre_hash.into(), &seal, target),
            "seal bound to wrong header accepted"
        );
    }
}
