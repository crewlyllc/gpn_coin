//! Service and ServiceFactory implementation for the GPN PoW node.

use codec::Decode;
use futures::FutureExt;
use gpn_runtime::{self, Block, RuntimeApi};
use sc_basic_authorship::ProposerFactory;
use sc_client_api::Backend;
use sc_client_api::BlockBackend;
use sc_client_api::StorageProvider;
use sc_consensus::{BlockImport, LongestChain};
use sc_consensus_pow::{start_mining_worker, PowAlgorithm, PowBlockImport, PowImportQueue};
use sc_network::config::MultiaddrWithPeerId;
use sc_network::{NetworkPeers, NetworkStateInfo, NetworkStatusProvider};
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
/// The miner's AccountId (32 bytes) appended so the runtime can pay the block
/// PRODUCER (Bitcoin-style coinbase). Legacy/external seals without it still
/// verify (author then unknown → beneficiary fallback).
const SEAL_AUTHOR_LEN: usize = 32;
/// Legacy seal (nonce + mix) — the minimum `verify_seal` accepts.
const SEAL_MIN_LEN: usize = SEAL_NONCE_LEN + SEAL_MIX_LEN;
const SEAL_LEN: usize = SEAL_NONCE_LEN + SEAL_MIX_LEN + SEAL_AUTHOR_LEN;

/// Lazily-built Ethash light cache for the dev epoch. Verification on every
/// block import and mining both share this cache.
pub(crate) fn ethash_cache() -> &'static [u8] {
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
pub(crate) fn ethash_full_size() -> usize {
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
    // Accept a legacy seal (nonce+mix = 40 bytes) or one that also carries the
    // miner's account (72 bytes). Reject anything else.
    if seal.len() != SEAL_MIN_LEN && seal.len() != SEAL_LEN {
        return false;
    }
    let nonce = u64::from_le_bytes(seal[..SEAL_NONCE_LEN].try_into().unwrap());
    let mut mix = [0u8; SEAL_MIX_LEN];
    mix.copy_from_slice(&seal[SEAL_NONCE_LEN..SEAL_MIN_LEN]);

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

    fn difficulty(
        &self,
        parent: <Block as BlockT>::Hash,
    ) -> Result<Self::Difficulty, sc_consensus_pow::Error<Block>> {
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
fn create_inherent_data_providers() -> impl Fn(
    <Block as BlockT>::Hash,
    (),
) -> futures::future::BoxFuture<
    'static,
    Result<sp_timestamp::InherentDataProvider, Box<dyn std::error::Error + Send + Sync>>,
> {
    move |_parent: <Block as BlockT>::Hash, _data: ()| {
        Box::pin(async move { Ok(sp_timestamp::InherentDataProvider::from_system_time()) })
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
        task_manager
            .spawn_handle()
            .spawn("telemetry", None, worker.run());
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

    let algorithm = GPNPowAlgorithm {
        client: client.clone(),
    };

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

/// Merge every peer-discovery source into `config.network.boot_nodes` before the
/// network is built. Sources, in priority order:
/// 1. the already-configured static bootnodes (`--bootnodes` / chain-spec /
///    compiled-in registry) — kept as the hard fallback;
/// 2. the persisted peer store (`<base-path>/gpn-peers.json`) — Bitcoin-style
///    addrman, so known-good peers are re-dialed on restart;
/// 3. `--bootnodes-dns` DNS seeds;
/// 4. `--bootnodes-url` published seed lists.
///
/// Every extra source is best-effort: failures are logged and ignored, so a
/// dead DNS seed or unreachable seed URL can never abort startup.
pub async fn prepare_network_config(
    mut config: Configuration,
    sources: &crate::discovery::SeedSources,
) -> Configuration {
    let base = config.base_path.path().to_path_buf();

    // Existing bootnodes + the compiled-in registry fallback. The registry
    // carries both the legacy `bootnodes[]` list and "Source A" — the
    // hardcoded ≤5 `bootstrap_seeds[]`, which are a *floor*: they must work
    // with no DNS, no HTTP and no chain (see design §3.0/§3.1).
    let registry = crate::network_registry::load_registry();
    let mut existing: Vec<String> = config
        .network
        .boot_nodes
        .iter()
        .map(|b| b.to_string())
        .collect();
    existing.extend(registry.bootnodes);
    existing.extend(crate::discovery::bootstrap_seed_addrs(
        &registry.bootstrap_seeds,
    ));

    // Resolve each extra source independently (each returns Result, never panics).
    let mut results: Vec<Result<Vec<String>, String>> = Vec::new();
    for name in &sources.dns {
        results.push(crate::discovery::resolve_dns_seed(name).await);
    }
    for url in &sources.urls {
        results.push(crate::discovery::fetch_seed_url(url).await);
    }
    // The persisted peer store acts as one more (always-available) source.
    results.push(Ok(crate::discovery::read_persisted_peers(&base)));

    let merged = crate::discovery::merge_sources_lossy(&existing, results);
    let bootnodes: Vec<MultiaddrWithPeerId> =
        merged.iter().filter_map(|s| s.parse().ok()).collect();

    log::info!(
        "peer-discovery: {} bootnode(s) after merging static + persisted + {} DNS + {} URL source(s)",
        bootnodes.len(),
        sources.dns.len(),
        sources.urls.len()
    );
    if bootnodes.is_empty() {
        log::warn!(
            "peer-discovery: no dialable bootnodes — this node may be unable to join the \
             network. Configure --bootnodes, --bootnodes-dns or --bootnodes-url."
        );
    }

    config.network.boot_nodes = bootnodes;
    config
}

/// Read the GPN on-chain peer registry: every endpoint declared by a registered
/// full node (`gpn-liveness::Endpoints`) and every registered compute provider
/// (`ComputeMarketplace::ProviderEndpoints`), converted to a multiaddress.
///
/// Best-effort: any read error yields fewer (or zero) endpoints, never a panic.
fn read_chain_endpoints(client: &FullClient) -> Vec<String> {
    use sc_client_api::StorageProvider;

    let info = client.chain_info();
    let mut out = Vec::new();
    let maps: [(&[u8], &[u8]); 2] = [
        (b"Liveness", b"Endpoints"),
        (b"ComputeMarketplace", b"ProviderEndpoints"),
    ];
    for (pallet, item) in maps {
        let prefix = StorageKey(crate::discovery::storage_prefix(pallet, item));
        match client.storage_pairs(info.best_hash, Some(&prefix), None) {
            Ok(pairs) => {
                for (_key, value) in pairs {
                    if let Some(bytes) = crate::discovery::decode_bytes_value(&value.0) {
                        if let Ok(text) = String::from_utf8(bytes) {
                            if let Some(ma) = crate::discovery::endpoint_to_multiaddr(&text) {
                                out.push(ma);
                            }
                        }
                    }
                }
            }
            Err(e) => log::debug!("chain-discovery: read of {pallet:?}::{item:?} failed: {e}"),
        }
    }
    out
}

/// Force the "a GPN node always mines" invariants onto a [`Configuration`]:
/// the Authority role AND `force_authoring`. Idempotent.
///
/// Applied on the CLI runner config in `command.rs` (so the startup banner
/// prints `Role: AUTHORITY` before anything reads it) and again here in
/// [`new_full`] for every other service entry point.
pub(crate) fn force_mining(config: &mut sc_service::Configuration) {
    config.role = sc_service::Role::Authority;
    config.force_authoring = true;
}

/// The **single decision** that gates the seal-producing miner.
///
/// A GPN node whose role is `Authority` always mines: the block *proposer*
/// (the `sc_consensus_pow` mining worker) is started under exactly this
/// condition, so the seal-producing *miner* MUST be started under it too. If
/// the two are gated on different predicates the node exhibits the classic PoW
/// gate bug — the proposer logs `Prepared block for proposing at #N` forever
/// while no miner exists to seal it, and the node idles at the same best block.
///
/// `--force-authoring` is deliberately **not** part of this decision. On this
/// Substrate rev sc-service never reads `Configuration::force_authoring` for a
/// PoW node (it only feeds Aura/Babe), so it is an *unused, CLI-derived* input
/// that can silently diverge from the role: `--alice`/`--validator`/a keyring
/// set `role = Authority` but leave `force_authoring = false`, which previously
/// left the proposer running with no miner to seal behind it.
pub(crate) fn miner_should_run(role: sc_service::Role) -> bool {
    role.is_authority()
}

/// Builds a new service for a full client.
pub async fn new_full<
    N: sc_network::NetworkBackend<Block, <Block as sp_runtime::traits::Block>::Hash>,
>(
    config: Configuration,
) -> Result<TaskManager, ServiceError> {
    // ── A GPN node ALWAYS mines: mining is structural, not an opt-in flag. ──
    // Just like "a node is always a provider", every node is also a miner.
    // We force the authority role + block authoring here, so a *plain*
    // `gpn-node` launch (no `--force-authoring`, no `--validator`, no keyring)
    // mines and serves blocks by default. Substrate otherwise derives the role
    // from `--validator`/`--dev`/a keyring only, which left a fresh box at
    // `Role: FULL` → no mining worker, no GPU-miner lines, no rewards.
    // The flags remain *accepted* (harmless) for backward compatibility.
    let mut config = config;
    force_mining(&mut config);

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

    // Captured before `config` is moved: used by the persistent peer store.
    let base_path = config.base_path.path().to_path_buf();

    // ---- Automatic NAT port mapping (design §5.1 step 3, §5.5) -----------
    // A home node behind a NAT router cannot be dialable without a forwarded
    // port. Rather than making every operator find their router admin page, we
    // ask the router (UPnP-IGD, then NAT-PMP) to open exactly one TCP port.
    //
    // This runs BEFORE `build_network` on purpose: libp2p's announced address
    // set is fixed at construction, so the external `ip:port` the router
    // reports has to be known now to be advertised. The learnt address is also
    // the address the dialability gate then tries to confirm by observation.
    //
    // Consent: on by default (`GPN_UPNP=auto`), never silent (one log line on
    // attempt, one on the result), fully disabled by `GPN_UPNP=off`/`--no-upnp`.
    let mut gate_cfg = crate::dialability::GateConfig::from_env(&config.network.public_addresses);
    let mut portmap_session: Option<crate::portmap::MappingSession> = None;
    {
        let listen_port = crate::dialability::listen_port(&config.network.listen_addresses)
            .unwrap_or(crate::dialability::DEFAULT_SEED_PORT);
        let pm_cfg = crate::portmap::PortMapConfig::from_env(Some(listen_port));
        let configured = gate_cfg.public_addr.clone();
        if !gate_cfg.require {
            log::debug!("portmap: skipped (dialability probe disabled)");
        } else if !gate_cfg.discoverable {
            log::debug!("portmap: skipped (--no-discoverable / GPN_DISCOVERABLE=0)");
        } else if configured.is_some() {
            // Ladder step 1: an explicit address wins; never touch the router.
            log::debug!(
                "portmap: skipped - GPN_PUBLIC_ADDR/--public-addr {} is configured",
                configured
                    .as_ref()
                    .map(|a| a.to_string())
                    .unwrap_or_default()
            );
        } else if !pm_cfg.enabled {
            // `GPN_UPNP=off` / `--no-upnp`: never contact the router.
            crate::portmap::log_outcome(&crate::portmap::MapOutcome::Disabled, pm_cfg.port);
        } else {
            match crate::dialability::detect_lan_ipv4() {
                Some(lan) => {
                    let session = crate::portmap::start(&pm_cfg, lan).await;
                    crate::portmap::log_outcome(&session.outcome, pm_cfg.port);
                    if let Some(mapping) = session.mapping() {
                        // Advertise the router-reported external ip:port. This
                        // is correct even when external != internal port, and
                        // it becomes the address the gate must see observed.
                        let advertised = crate::portmap::advertised_address(
                            gate_cfg.public_addr.as_ref(),
                            Some(mapping),
                        );
                        if let Some(addr) = advertised {
                            if !config.network.public_addresses.contains(&addr) {
                                config.network.public_addresses.push(addr.clone());
                            }
                            log::info!(
                                "portmap: advertising public address {addr} (from {} mapping)",
                                mapping.method
                            );
                            // The gate must verify THIS address, not a stale one.
                            gate_cfg.public_addr = Some(addr);
                        }
                    }
                    portmap_session = Some(session);
                }
                None => {
                    log::warn!(
                        "portmap: no private/public LAN IPv4 found - cannot ask the router to \
                         forward TCP {}; skipping automatic mapping",
                        pm_cfg.port
                    );
                }
            }
        }
    }

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

    // ---- Keep the NAT mapping alive, and re-detect a router when there is
    // none (design §5.5 / §5.3) ------------------------------------------
    // A live rule is renewed at half its lease. When there is **no** rule the
    // loop re-runs UPnP-IGD -> NAT-PMP discovery every 10 minutes, so an
    // operator who enables UPnP after starting the node, or a router that
    // rebooted and dropped the rule, recovers without a node restart.
    // Logging is edge-triggered inside the loop (no per-retry nag).
    if let Some(session) = portmap_session {
        if session.config.enabled {
            task_manager
                .spawn_handle()
                .spawn("gpn-portmap-maintain", None, async move {
                    crate::portmap::run_maintenance(session).await;
                });
        }
    }

    // ---- Network identity banner + peer guard ----------------------------
    // (design-network-identity-fork-tolerance.md). Print chain-id + genesis +
    // spec_version, classify the running chain against the canonical registry,
    // and warn loudly — never forbid — when isolated (0 peers) or outdated.
    // Explicitly NO 0-peer mining ban: identity is defined by genesis + chain-id.
    {
        let info = client.chain_info();
        let spec_version = gpn_runtime::VERSION.spec_version;
        let running_version = env!("CARGO_PKG_VERSION");
        let genesis_hex = format!("{:?}", info.genesis_hash);
        let st = crate::network_registry::classify(&genesis_hex, spec_version, running_version);
        log::info!("==============================================================");
        log::info!("GPN network identity");
        log::info!("  network  : {}", st.name);
        log::info!("  chain-id : {}", st.chain_id);
        log::info!("  genesis  : {}", genesis_hex);
        log::info!("  spec     : {} (node v{})", spec_version, running_version);
        log::info!("  identity : {}", st.summary);
        log::info!("==============================================================");
    }
    // ---- Dialability: capability probe, NOT a membership gate (queue #46) ----
    // Dialable is **REQUIRED** by default: the network is still growing and
    // every node is expected to be part of the backbone, so a node that cannot
    // be dialled prints the full operator notice and exits EXIT_NOT_DIALABLE.
    // `--no-require-backbone` / `GPN_REQUIRE_BACKBONE=0` opts out into the old
    // soft mode: run degraded, re-check every 10 min, print NOW DIALABLE once
    // the forward works (design §5.3).
    {
        if gate_cfg.require {
            log::info!(
                "dialability: probing inbound reachability within {}s — seed={} discoverable={} \
                 require_backbone={} (GPN_PUBLIC_ADDR={})",
                gate_cfg.timeout.as_secs(),
                gate_cfg.seed,
                gate_cfg.discoverable,
                gate_cfg.require_backbone,
                gate_cfg
                    .public_addr
                    .as_ref()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|| "unset".into()),
            );
            let verdict = crate::dialability::wait_for_dialability(&network, &gate_cfg).await;
            if verdict.dialable {
                log::info!(
                    "✅ dialability: this node IS dialable — {} ({}) [tier: {}]",
                    verdict
                        .evidence
                        .as_ref()
                        .map(|e| e.to_string())
                        .unwrap_or_default(),
                    verdict.detail,
                    gate_cfg.intended_tier(true),
                );
            } else {
                let hard = gate_cfg.require_backbone;
                let port = crate::dialability::listen_port(&network.listen_addresses())
                    .unwrap_or(crate::dialability::DEFAULT_SEED_PORT);
                match crate::dialability::degradation_notice(
                    crate::dialability::detect_lan_ipv4(),
                    port,
                    gate_cfg.seed,
                    hard,
                ) {
                    Some(notice) => {
                        for line in notice.lines() {
                            if hard {
                                log::error!("{line}");
                            } else {
                                log::warn!("{line}");
                            }
                        }
                    }
                    // Only reachable with --no-seed; never fail silently.
                    None => log::error!(
                        "dialability: this node is NOT dialable ({}) — see \
                         https://gpncoin.ai/docs/run-a-node#make-your-node-dialable",
                        verdict.detail
                    ),
                }

                if hard {
                    // Default path: refuse to run a node nobody can dial.
                    log::error!("   reason: {}", verdict.detail);
                    // Distinct exit code so `gpn_node` can report
                    // `node=down(not dialable)` instead of a generic crash.
                    std::process::exit(crate::dialability::EXIT_NOT_DIALABLE);
                }

                // Soft mode (--no-require-backbone): keep syncing/mining/serving.
                log::warn!(
                    "dialability: continuing in degraded mode [tier: {}] — participation is \
                     unaffected",
                    gate_cfg.intended_tier(false)
                );

                // Periodic re-check (design §5.3): edge-triggered only. The startup
                // verdict seeds `last_dialable`, so nothing repeats until the state
                // actually changes (not dialable -> dialable prints NOW DIALABLE).
                let watch_cfg = gate_cfg.clone();
                let watch_net = network.clone();
                let start_dialable = verdict.dialable;
                task_manager
                    .spawn_handle()
                    .spawn("gpn-dialability-watch", None, async move {
                        crate::dialability::watch_dialability(watch_net, watch_cfg, start_dialable)
                            .await;
                    });
            }
        } else {
            log::warn!(
                "dialability: probe DISABLED (GPN_REQUIRE_DIALABLE=0) — inbound \
                 reachability is not checked, so require_backbone={} cannot be enforced; \
                 local/single-node testing only",
                gate_cfg.require_backbone
            );
        }
    }

    // ---- Clock / NTP skew check (roadmap item 8) -------------------------
    // A skewed wall clock can wedge a node (block timestamps + timing-based
    // difficulty reject otherwise-valid blocks; the Windows box was ~57 s off).
    // Best-effort, non-fatal, and non-blocking: runs once on a background task.
    task_manager
        .spawn_handle()
        .spawn("gpn-clock-check", None, async move {
            let _ = tokio::task::spawn_blocking(crate::clock::run_startup_check).await;
        });
    {
        let net = network.clone();
        task_manager
            .spawn_handle()
            .spawn("gpn-peer-watch", None, async move {
                let mut tick = tokio::time::interval(Duration::from_secs(30));
                tick.tick().await; // consume the immediate first tick
                loop {
                    tick.tick().await;
                    if net.sync_num_connected() == 0 {
                        log::warn!(
                            "⚠️ No peers — this node is NOT connected to the GPN network. \
                             Network identity is defined by genesis + chain-id, not by peer \
                             count; mining continues (no 0-peer ban)."
                        );
                    }
                }
            });
    }

    // ---- Persistent peer store (Bitcoin-style addrman) -------------------
    // Snapshot the addresses of peers we are *connected to* into
    // `<base-path>/gpn-peers.json`. On the next start these are merged into the
    // bootnode set (see `prepare_network_config`), so known-good peers are
    // re-dialed even if the static bootnodes are down.
    {
        let net = network.clone();
        let base = base_path.clone();
        task_manager
            .spawn_handle()
            .spawn("gpn-peer-store", None, async move {
                let mut tick = tokio::time::interval(Duration::from_secs(60));
                loop {
                    tick.tick().await;
                    let Ok(state) = net.network_state().await else {
                        continue;
                    };
                    let mut addrs = Vec::new();
                    for (peer, info) in &state.connected_peers {
                        for addr in &info.known_addresses {
                            // `known_addresses` omit the peer id; add it back so the
                            // stored entry is a dialable bootnode.
                            addrs.push(format!("{addr}/p2p/{peer}"));
                        }
                    }
                    if !addrs.is_empty() {
                        if let Err(e) = crate::discovery::write_persisted_peers(&base, &addrs) {
                            log::debug!("peer-store: persist failed: {e}");
                        }
                    }
                }
            });
    }

    // ---- On-chain discovery (GPN-native top-up) --------------------------
    // Read the full-node registry + provider endpoints from chain and dial
    // them. Bootstrap chicken-and-egg: wait until we have at least one peer
    // (bounded) before the first scan; never blocks startup.
    {
        let net = network.clone();
        let client = client.clone();
        task_manager
            .spawn_handle()
            .spawn("gpn-chain-discovery", None, async move {
                {
                    let mut gate = tokio::time::interval(Duration::from_secs(30));
                    for _ in 0..10 {
                        if net.sync_num_connected() > 0 {
                            break;
                        }
                        gate.tick().await;
                    }
                }
                let mut tick = tokio::time::interval(Duration::from_secs(600));
                loop {
                    tick.tick().await;
                    let endpoints = read_chain_endpoints(&client);
                    let mut added = 0usize;
                    for ma in endpoints {
                        if let Ok(mw) = ma.parse::<MultiaddrWithPeerId>() {
                            net.add_known_address(mw.peer_id, mw.multiaddr);
                            added += 1;
                        }
                    }
                    if added > 0 {
                        log::info!("chain-discovery: dialing {added} on-chain peer(s)");
                    } else {
                        log::debug!("chain-discovery: no dialable on-chain endpoints");
                    }
                }
            });
    }

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
            offchain_workers
                .run(client.clone(), task_manager.spawn_handle())
                .boxed(),
        );
    }

    let role = config.role;
    let prometheus_registry = config.prometheus_registry().cloned();

    let rpc_extensions_builder = {
        let client = client.clone();
        let miner_worker_slot = miner_worker_slot.clone();
        let pool = transaction_pool.clone();
        Box::new(move |_| {
            Ok(crate::rpc::create_full_rpc(
                miner_worker_slot.clone(),
                client.clone(),
                pool.clone(),
            ))
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
            // The node's payout account goes in the block's PRE-runtime digest, so
            // it's known deterministically at execution time → the block producer
            // is paid (Bitcoin-style coinbase), gated by attestation.
            mining_account(keystore_container.keystore().as_ref()).map(|a| a.to_vec()),
            create_inherent_data_providers(),
            Duration::from_secs(2),
            Duration::from_secs(1),
        );

        task_manager
            .spawn_essential_handle()
            .spawn("pow-mining-worker", None, task);

        // Park a clone of the worker in the shared slot so external miners can
        // fetch templates and submit seals over RPC.
        *miner_worker_slot.lock().unwrap() = Some(worker.clone());

        // Wire the seal-producing miner from the SAME predicate that started
        // the proposer above (`miner_should_run(role)`), NOT from the unused
        // `--force-authoring` flag. A plain `gpn-node` — including one launched
        // with `--alice`/`--validator`/a keyring, which set role=Authority but
        // leave force_authoring=false — therefore always gets a miner to seal
        // the blocks its proposer builds.
        if miner_should_run(role) {
            // The node's own wallet is its block-reward payout address (Bitcoin-style).
            let author = mining_account(keystore_container.keystore().as_ref());
            log::info!(
                "⛏  Mining enabled (role={role}): starting seal-producing miner 
                 (no --force-authoring required)"
            );
            spawn_miner(worker, author);
        }
    }

    Ok(task_manager)
}

pub(crate) type MiningWorker =
    sc_consensus_pow::MiningHandle<Block, GPNPowAlgorithm, Arc<SyncingService<Block>>>;

/// How many nonces the GPU searches per `worker.metadata()` poll. Large enough
/// to find a dev-difficulty solution in one shot, but bounded so we regularly
/// re-check for a newer block build.
const GPU_BATCH: usize = 1 << 20;

/// How long the miner parks (and re-checks the yield flag) while an AI job runs.
/// While the flag is held the miner does **no** nonce search at all (full stop);
/// this is only the poll interval, so mining resumes within this bound once the
/// job signals completion (kept short for a snappy resume).
const GPU_YIELD_SLEEP_MS: u64 = 500;

/// True when the local provider has asked the GPU to yield (an inference job is
/// running). A file flag lets the separate provider process signal us with no IPC:
/// presence of `$GPN_GPU_YIELD` or `~/.gpn/gpu-yield`.
fn gpu_yield_requested() -> bool {
    if let Ok(p) = std::env::var("GPN_GPU_YIELD") {
        return std::path::Path::new(&p).exists();
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::Path::new(&format!("{home}/.gpn/gpu-yield")).exists()
}

/// Spawn a dedicated miner thread: poll `worker.metadata()`, run real Ethash
/// mining to find a valid nonce + mix_hash, then submit the seal. Runs on its
/// own OS thread because the submit future holds a non-`Send` mutex guard
/// across an await.
///
/// Tries the **GPU OpenCL miner first** (full dataset on the GPU). If no
/// OpenCL GPU is available, falls back to the light-cache CPU miner so the
/// node still produces blocks on machines without a GPU.
fn spawn_miner(worker: MiningWorker, author: Option<[u8; 32]>) {
    log::info!(
        "⛏  miner start: probing GPU (device 0); author={} — will seal every block the proposer builds",
        match author {
            Some(_) => "set",
            None => "none (runtime BlockRewardBeneficiary fallback)",
        }
    );
    match EthashGpuMiner::new(0) {
        Ok(_) => spawn_gpu_miner(worker, author),
        Err(e) => {
            log::warn!("GPU miner unavailable ({e}); falling back to CPU light-cache miner");
            spawn_cpu_miner(worker, author);
        }
    }
}

/// Append the miner's account to a legacy (40-byte) seal so the runtime can
/// credit the block producer. No-op if the seal already carries it or no author
/// is known.
fn seal_with_author(mut seal: Vec<u8>, author: Option<[u8; 32]>) -> Vec<u8> {
    if let Some(a) = author {
        if seal.len() == SEAL_MIN_LEN {
            seal.extend_from_slice(&a);
        }
    }
    seal
}

/// The node's own mining payout account: `GPN_MINER_ACCOUNT` (0x-hex) if set,
/// else the first sr25519 key in its keystore. `None` → the runtime falls back to
/// the configured `BlockRewardBeneficiary`.
fn mining_account(keystore: &dyn sp_keystore::Keystore) -> Option<[u8; 32]> {
    if let Ok(h) = std::env::var("GPN_MINER_ACCOUNT") {
        if let Ok(b) = hex::decode(h.trim_start_matches("0x")) {
            if let Ok(a) = <[u8; 32]>::try_from(b.as_slice()) {
                return Some(a);
            }
        }
    }
    for id in [*b"aura", *b"gran", *b"babe", *b"gpu!", *b"acco", *b"imol"] {
        if let Some(p) =
            sp_keystore::Keystore::sr25519_public_keys(keystore, sp_core::crypto::KeyTypeId(id))
                .first()
        {
            return Some(p.0);
        }
    }
    None
}

/// GPU miner: build the full Ethash dataset on the GPU once, then loop over
/// `worker.metadata()` and search nonce batches in parallel on the GPU.
/// The node's `verify_seal` recomputes with `hashimoto_light`, so the GPU's
/// full-DAG solution is accepted as long as mix + result match (they do — the
/// miner self-validates against the `ethash` crate reference).
fn spawn_gpu_miner(worker: MiningWorker, author: Option<[u8; 32]>) {
    std::thread::Builder::new()
        .name("gpn-gpu-miner".into())
        .spawn(move || {
            let miner = match EthashGpuMiner::new(0) {
                Ok(m) => m,
                Err(e) => {
                    // Never leave the proposer without a miner: fall back to CPU.
                    log::error!("GPU miner init failed: {e}; falling back to CPU light-cache miner");
                    spawn_cpu_miner(worker, author);
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
                    // e.g. OpenCL OOM when the GPU is busy with an inference job:
                    // keep producing blocks via the CPU miner rather than idling.
                    log::error!(
                        "GPU DAG build failed: {e}; falling back to CPU light-cache miner"
                    );
                    spawn_cpu_miner(worker, author);
                    return;
                }
            };

            let mut last_pre_hash: Option<<Block as BlockT>::Hash> = None;
            let mut nonce: u64 = 0;
            loop {
                // AI-compute full stop: while a local inference job runs the
                // provider holds a flag; we do NOT search any nonces at all so the
                // GPU is dedicated to the job, then resume the instant the flag
                // clears. The miner thread stays alive (we only park here), so
                // mining resumes immediately with no process/thread restart.
                // One GPU = one job: mining and inference never contend.
                if gpu_yield_requested() {
                    std::thread::sleep(Duration::from_millis(GPU_YIELD_SLEEP_MS));
                    continue;
                }
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
                            let seal_bytes = seal_with_author(seal.to_seal_bytes(), author);
                            let nonce_found = seal.nonce;
                            let ok = futures::executor::block_on(worker.submit(seal_bytes));
                            if ok {
                                log::info!(
                                    "⛏  GPU sealed block: nonce={nonce_found} pre_hash={pre_hash:?}"
                                );
                            } else {
                                log::warn!(
                                    "GPU seal for pre_hash={pre_hash:?} (nonce={nonce_found}) was rejected by the node; retrying"
                                );
                            }
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
fn spawn_cpu_miner(worker: MiningWorker, author: Option<[u8; 32]>) {
    std::thread::Builder::new()
        .name("gpn-cpu-miner".into())
        .spawn(move || {
            let cache = ethash_cache();
            let full_size = ethash_full_size();
            log::info!("CPU light-cache miner online (epoch {DEV_EPOCH}); no GPU backend");
            let mut nonce: u64 = 0;
            loop {
                // AI-compute full stop (same contract as the GPU miner): while a
                // provider holds the yield flag we search no nonces at all, then
                // resume the instant it clears. Thread stays alive throughout.
                if gpu_yield_requested() {
                    std::thread::sleep(Duration::from_millis(GPU_YIELD_SLEEP_MS));
                    continue;
                }
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
                            if let Some(a) = author {
                                seal.extend_from_slice(&a);
                            }
                            let nonce_found = nonce;
                            let ok = futures::executor::block_on(worker.submit(seal));
                            if ok {
                                log::info!(
                                    "⛏  CPU sealed block: nonce={nonce_found} pre_hash={pre_hash:?}"
                                );
                            } else {
                                log::warn!(
                                    "CPU seal for pre_hash={pre_hash:?} (nonce={nonce_found}) was rejected by the node; retrying"
                                );
                            }
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
        assert_eq!(seal.len(), SEAL_MIN_LEN, "legacy seal is nonce+mix");
        assert!(
            verify_seal(&pre_hash.into(), &seal, target),
            "valid seal rejected"
        );
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
        assert!(
            !verify_seal(&pre_hash.into(), &seal, target),
            "corrupt mix accepted"
        );
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
        let short: Vec<u8> = good[..SEAL_MIN_LEN - 1].to_vec();
        assert!(
            !verify_seal(&pre_hash.into(), &short, target),
            "short seal accepted"
        );

        // Too long (41 bytes).
        let mut long = good.clone();
        long.push(0u8);
        assert!(
            !verify_seal(&pre_hash.into(), &long, target),
            "long seal accepted"
        );
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
        assert!(
            !verify_seal(&pre_hash.into(), &seal, hard),
            "too-hard seal accepted"
        );
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

#[cfg(test)]
mod mining_gate_tests {
    use super::*;
    use sc_service::Role;

    /// THE regression this fix addresses: a node whose role is Authority must
    /// spawn the seal-producing miner **even with no `--force-authoring`**.
    ///
    /// Before the fix the miner spawn was gated on `config.force_authoring`,
    /// a separate (and, for PoW, *unused*) CLI-derived flag. `--alice` /
    /// `--validator` / a keyring set role=Authority but leave
    /// force_authoring=false, so the proposer ran while no miner existed to
    /// seal its proposals → the node idled at the same best block forever.
    #[test]
    fn authority_without_force_authoring_still_runs_miner() {
        // The decision ignores force_authoring entirely; it is driven by role.
        assert!(
            miner_should_run(Role::Authority),
            "an Authority node must always spawn a miner (no --force-authoring needed)"
        );
    }

    /// A plain full node does not mine (the proposer is not started either).
    #[test]
    fn full_node_does_not_run_miner() {
        assert!(!miner_should_run(Role::Full));
    }
}
