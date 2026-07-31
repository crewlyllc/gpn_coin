//! Service and ServiceFactory implementation for the GPN PoW node.

use futures::FutureExt;
use codec::Encode;
use gpn_runtime::{self, Block, RuntimeApi};
use sc_basic_authorship::ProposerFactory;
use sc_client_api::BlockBackend;
use sc_client_api::Backend;
use sc_consensus::{BlockImport, LongestChain};
use sc_consensus_pow::{PowAlgorithm, PowBlockImport, PowImportQueue, start_mining_worker};
use sc_network_sync::SyncingService;
use sc_service::{error::Error as ServiceError, Configuration, TaskManager};
use sc_telemetry::{Telemetry, TelemetryWorker};
use sc_transaction_pool_api::OffchainTransactionPoolFactory;
use sp_consensus_pow::{Seal, POW_ENGINE_ID};
use sp_core::U256;
use sp_runtime::generic::BlockId;
use sp_runtime::traits::Block as BlockT;
use std::{sync::Arc, time::Duration};

pub(crate) type FullClient = sc_service::TFullClient<
    Block,
    RuntimeApi,
    sc_executor::WasmExecutor<sp_io::SubstrateHostFunctions>,
>;
type FullBackend = sc_service::TFullBackend<Block>;
type FullSelectChain = LongestChain<FullBackend, Block>;

/// The minimum difficulty target for a valid PoW solution.
///
/// NOTE: This is a *dev/testnet* placeholder value chosen so the CPU miner can
/// actually find a seal. The value is a 256-bit *target* (hash < target = valid),
/// so HIGHER = EASIER. The old value 1_000_000_000 made every valid hash astronomically
/// unlikely (P ~ 1e-68) — the miner could never author a block. 2^240 gives
/// P ~ 1.5e-5 per hash, so a CPU finds a block in tens of thousands of hashes.
/// A real GPU Ethash target + difficulty adjustment (see `pallet-gpu-pow`) replaces
/// this placeholder before any public network.
fn initial_difficulty() -> U256 {
    // 2^240, expressed as big-endian: byte[1] = 0x01, rest 0.
    let mut buf = [0u8; 32];
    buf[1] = 0x01;
    U256::from_big_endian(&buf)
}

/// A simple CPU PoW algorithm for GPN.
///
/// NOTE: This is a placeholder mining algorithm so the node can compile and
/// author blocks on a dev chain. The real GPU Ethash-derived algorithm is a
/// follow-up (see `pallet-gpu-pow` + `gpn-agent`).
#[derive(Clone)]
pub struct GPNPowAlgorithm;

fn hash_meets_difficulty(pre_hash: &<Block as BlockT>::Hash, seal: &Seal, difficulty: U256) -> bool {
    let seal_hash = sp_io::hashing::blake2_256(seal);
    let work = (pre_hash.encode(), seal_hash).encode();
    let hash = sp_io::hashing::blake2_256(&work);
    U256::from_little_endian(&hash) < difficulty
}
impl PowAlgorithm<Block> for GPNPowAlgorithm {
    type Difficulty = U256;

    fn difficulty(&self, _parent: <Block as BlockT>::Hash) -> Result<Self::Difficulty, sc_consensus_pow::Error<Block>> {
        Ok(initial_difficulty())
    }

    fn verify(
        &self,
        _parent: &BlockId<Block>,
        pre_hash: &<Block as BlockT>::Hash,
        _pre_digest: Option<&[u8]>,
        seal: &Seal,
        difficulty: Self::Difficulty,
    ) -> Result<bool, sc_consensus_pow::Error<Block>> {
        Ok(hash_meets_difficulty(pre_hash, seal, difficulty))
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
    (GPNPowAlgorithm, Option<Telemetry>),
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

    let algorithm = GPNPowAlgorithm;

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
        other: (algorithm, telemetry),
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
        other: (algorithm, mut telemetry),
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
        Box::new(move |_| Ok(jsonrpsee::RpcModule::new(())))
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

        if force_authoring {
            spawn_miner(worker);
        }
    }

    Ok(task_manager)
}

type MiningWorker = sc_consensus_pow::MiningHandle<Block, GPNPowAlgorithm, Arc<SyncingService<Block>>>;

/// Spawn a dedicated miner thread: poll `worker.metadata()`, hash-search for a
/// valid seal, then submit it. Runs on its own OS thread because the submit
/// future holds a non-`Send` mutex guard across an await.
fn spawn_miner(worker: MiningWorker) {
    std::thread::Builder::new()
        .name("gpn-miner".into())
        .spawn(move || {
            let mut nonce: u64 = 0;
            loop {
                if let Some(metadata) = worker.metadata() {
                    let pre_hash = metadata.pre_hash;
                    let difficulty = metadata.difficulty;
                    // Search a bounded window so we re-check for a new build.
                    for _ in 0..100_000 {
                        let seal: Seal = nonce.to_le_bytes().to_vec();
                        if hash_meets_difficulty(&pre_hash, &seal, difficulty) {
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
        .expect("failed to spawn miner thread");
}
