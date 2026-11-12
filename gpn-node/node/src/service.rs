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
use sc_network::service::traits::{NotificationEvent, ValidationResult};
use sc_network::{NetworkPeers, NetworkStateInfo, NetworkStatusProvider, ReputationChange};
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

/// First four bytes of a hash, for a log line (`0x1234abcd`).
fn short_hash<H: std::fmt::Debug>(h: &H) -> String {
    let s = format!("{:?}", h);
    let s = s.trim_start_matches("0x");
    format!("0x{}", s.chars().take(8).collect::<String>())
}

/// Read our own chain state for the `gpn/chain/1` announcement (GPN #159).
///
/// `None` means the tip could not be read as a real 32-byte hash, in which case
/// the caller announces **nothing** — an unreadable tip must never be published
/// as a fact, and a stale height would be worse than silence (#141/#144 rule).
fn local_chain_state(client: &FullClient) -> Option<crate::chain_state::ChainState> {
    let info = client.chain_info();
    crate::chain_state::ChainState::new(
        &format!("{:?}", info.genesis_hash),
        &format!("{:?}", info.best_hash),
        u64::from(info.best_number),
    )
}

// ── Dialability state publication ────────────────────────────────────────
//
// The provider heartbeat (`gpn_provider_status`) publishes `multiaddrs` to the
// access point, but it is a SEPARATE process: it cannot see this node's
// dialability verdict (an inbound peer observed, a UPnP-IGD/NAT-PMP mapping the
// router confirmed, a STUN mapping). The gate therefore writes its conclusion
// to the state file the heartbeat reads (see
// [`crate::dialability::write_state_file`]). It carries the reachability tier
// too, so the AP can count backbone vs leaf and a leaf never republishes a
// dialable address.
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

/// Is a solved seal still worth submitting?
///
/// `sc-consensus-pow`'s `MiningHandle::submit` verifies the seal against the
/// **current** build's `pre_hash`, not the one it was solved for. On a chain
/// with more than one authority two miners routinely prepare a block at the
/// same height; the loser's solution then reaches `submit()` after the winner's
/// block has already been imported, so it fails verification and is logged as
/// `Unable to import mined block: seal is invalid` — which reads like a broken
/// sealer and has been mistaken for the cause of slow blocks. Such a seal can
/// never be useful: its parent is no longer the head, so the header (and hence
/// the hash the seal commits to) is dead. Drop it and re-read the template.
fn seal_still_wanted(
    solved_for: &<Block as BlockT>::Hash,
    live_template: Option<&<Block as BlockT>::Hash>,
) -> bool {
    live_template == Some(solved_for)
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

    // The refresh task reuses exactly these sources, so record them once.
    crate::discovery::set_active_sources(sources);

    // Resolve each extra source independently (each returns Result, never panics).
    let mut results: Vec<Result<Vec<String>, String>> = Vec::new();
    let mut from_network: Vec<String> = Vec::new();
    for name in &sources.dns {
        let r = crate::discovery::resolve_dns_seed(name).await;
        if let Ok(v) = &r {
            from_network.extend(v.clone());
        }
        results.push(r);
    }
    for url in &sources.urls {
        let r = crate::discovery::fetch_seed_url(url).await;
        if let Ok(v) = &r {
            from_network.extend(v.clone());
        }
        results.push(r);
    }
    // Fresh network results are the last-good cache for the next start: if the
    // publisher is down later, the node still knows who to dial (design §3.3).
    if !from_network.is_empty() {
        if let Err(e) = crate::discovery::write_seed_cache(&base, &from_network) {
            log::debug!("peer-discovery: seed cache write failed: {e}");
        }
    } else {
        // Nothing fetched: fall back to the cached list (bounded age).
        let cached =
            crate::discovery::read_seed_cache(&base, crate::discovery::SEED_CACHE_MAX_AGE_SECS);
        if !cached.is_empty() {
            log::info!(
                "peer-discovery: seed sources unavailable — using {} cached seed(s)",
                cached.len()
            );
            results.push(Ok(cached));
        }
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

    // ---- Tier 0 (oracle seeds) get reserved slots (design §1, GPN #154) ----
    // A reserved peer is exempt from the peer-count cap, is never evicted, and
    // the network layer keeps re-dialling it. Only the compiled-in seeds are
    // tier 0 — a peer learned from a seed list or the chain never is.
    // Our own peer id must never be an oracle seed: a node that reserves a slot
    // for itself dials itself, wastes the slot, and inflates the "tier-0
    // connected" count. Observed 2026-11-10 (box 1 listed its own id twice).
    let own_peer_id = config
        .network
        .node_key
        .clone()
        .into_keypair()
        .ok()
        .map(|k| k.public().to_peer_id().to_string());
    let oracle_ids: Vec<String> = crate::peer_priority::oracle_peer_ids(&registry.bootstrap_seeds)
        .into_iter()
        .filter(|id| own_peer_id.as_deref() != Some(id.as_str()))
        .collect();
    if config.network.default_peers_set.reserved_nodes.is_empty() && !oracle_ids.is_empty() {
        let reserved: Vec<MultiaddrWithPeerId> = config
            .network
            .boot_nodes
            .iter()
            .filter(|m| oracle_ids.iter().any(|id| id == &m.peer_id.to_string()))
            .cloned()
            .collect();
        if !reserved.is_empty() {
            log::info!(
                "peer-priority: {} tier-0 oracle seed(s) reserved (never evicted, re-dialled \
                 every {}s): {}",
                reserved.len(),
                crate::peer_priority::REDIAL_INTERVAL_SECS,
                oracle_ids.join(", ")
            );
            config.network.default_peers_set.reserved_nodes = reserved;
        }
    }

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

/// True for a development/local chain (`--dev`, `--alice`-style), where the
/// dialability gate must never be a reason to refuse to start.
fn is_dev_chain(config: &sc_service::Configuration) -> bool {
    use sc_chain_spec::ChainType;
    matches!(
        config.chain_spec.chain_type(),
        ChainType::Development | ChainType::Local
    ) || config.chain_spec.id().to_ascii_lowercase().contains("dev")
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

/// `GPN_STANDBY=1` — the supervisor (`gpn_node`) has established that this
/// build is **below the network's published `min_version`**.
///
/// In standby the node keeps syncing (its data stays useful, and it can be
/// upgraded in place) but it must not author or mine: an outdated node can
/// produce blocks that the upgraded network rejects, which looks like a chain
/// stall to everyone else. Announcing is suppressed by the supervisor with
/// `--no-seed --no-discoverable`.
///
/// Deliberately an env var and not a flag: it is set by the supervisor from a
/// *signature-verified* release manifest, never by hand-tuned CLI.
pub(crate) fn standby_requested() -> bool {
    matches!(
        std::env::var("GPN_STANDBY")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// The role this process actually runs with: standby downgrades an authority
/// to a plain full node, so neither the proposer nor the miner starts.
pub(crate) fn effective_role(requested: sc_service::Role, standby: bool) -> sc_service::Role {
    if standby {
        sc_service::Role::Full
    } else {
        requested
    }
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
    if standby_requested() {
        // Below the minimum version: sync only. Logged as a hard warning so it
        // shows up in journalctl and in the supervisor's [node] stream.
        log::warn!(
            "  UPGRADE REQUIRED (GPN_STANDBY=1): this node is below the network's \
             minimum version. It will SYNC but NOT MINE and NOT AUTHOR. Install the \
             current release (or set GPN_AUTO_UPDATE=1) and restart."
        );
        config.role = effective_role(config.role, true);
        config.force_authoring = false;
    } else {
        force_mining(&mut config);
    }

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

    // ---- IPv6: listen + announce (owner decision 2026-11-02) -----------
    // IPv6 is the answer to CGNAT: there is no NAT, so a node behind a
    // carrier-grade-NATed IPv4 line is still dialable when the ISP routes a
    // global prefix. Addressing and firewalling are separate gates — a global
    // address only makes the node a *candidate*; the same evidence gate below
    // must still pass before the address is published externally.
    let seed_port = crate::dialability::listen_port(&config.network.listen_addresses)
        .unwrap_or(crate::dialability::DEFAULT_SEED_PORT);
    let configured_v6: Option<sc_network::multiaddr::Multiaddr> = gate_cfg
        .public_addr
        .as_ref()
        .filter(|a| crate::dialability::is_v6_endpoint(a))
        .cloned();
    let mut detected_v6: Option<crate::dialability::V6Selection> = None;
    if gate_cfg.require && gate_cfg.discoverable {
        // An operator-configured `/ip6` or `/dns6` endpoint wins: announce it
        // verbatim (the gate validates it later). Still bind the wildcard v6
        // listener so inbound connections can arrive.
        if configured_v6.is_none() {
            detected_v6 = crate::dialability::detect_public_ipv6();
            if let Some(sel) = detected_v6.as_ref() {
                if sel.used_temporary {
                    log::warn!(
                        "dialability: IPv6 has only a TEMPORARY (privacy) address on {}; using \
                         it (it will rotate with the ISP's privacy timer) — pin a stable address \
                         or disable privacy extensions for a permanent endpoint",
                        sel.temporary_only_ifaces.join(", ")
                    );
                }
                if let Some(best) = sel.best() {
                    log::info!(
                        "dialability: IPv6 global address {} on '{}' (default-route iface={}, \
                         temporary={})",
                        best.addr,
                        best.iface,
                        best.default_route,
                        best.temporary
                    );
                }
            }
        }
        let have_v6 = configured_v6.is_some() || detected_v6.is_some();
        if have_v6 {
            config.network.listen_addresses = crate::dialability::plan_listen_addresses(
                &config.network.listen_addresses,
                true,
                seed_port,
            );
            // Candidate announcement inside libp2p. External publication (the
            // dialability state file read by the heartbeat) happens only after
            // the evidence gate below.
            let candidate = configured_v6.clone().or_else(|| {
                detected_v6
                    .as_ref()
                    .and_then(|s| s.addr())
                    .and_then(|a| crate::dialability::v6_base(&a, seed_port))
            });
            if let Some(addr) = candidate {
                if !config.network.public_addresses.contains(&addr) {
                    config.network.public_addresses.push(addr);
                }
            }
        }
    }
    // A GPN_PUBLIC_ADDR supplied via the environment is not in
    // `config.network.public_addresses` (only `--public-addr` is); announce it
    // so both stacks behave identically.
    if let Some(cfg_addr) = gate_cfg.public_addr.clone() {
        if !config.network.public_addresses.contains(&cfg_addr) {
            config.network.public_addresses.push(cfg_addr);
        }
    }

    let mut portmap_session: Option<crate::portmap::MappingSession> = None;
    // SPEC 47: the mapping outcome is carried into the degradation notice so
    // "the port is not open" always says WHY.
    let mut portmap_hint = crate::dialability::PortMapHint::Unknown;
    {
        let listen_port = seed_port;
        let pm_cfg = crate::portmap::PortMapConfig::from_env(Some(listen_port));
        let configured = gate_cfg.public_addr.clone();
        if !gate_cfg.require {
            log::debug!("portmap: skipped (dialability probe disabled)");
        } else if !gate_cfg.discoverable {
            log::debug!("portmap: skipped (--no-discoverable / GPN_DISCOVERABLE=0)");
        } else if configured.is_some() {
            // Ladder step 1: an explicit address wins; never touch the router.
            log::info!("portmap: skipped (public address configured)");
            portmap_hint = crate::dialability::PortMapHint::Skipped;
        } else if !pm_cfg.enabled {
            // `GPN_UPNP=off` / `--no-upnp`: never contact the router.
            crate::portmap::log_outcome(&crate::portmap::MapOutcome::Disabled, pm_cfg.port);
            portmap_hint = crate::dialability::PortMapHint::Disabled;
        } else {
            match crate::dialability::detect_lan_ipv4() {
                Some(lan) => {
                    let session = crate::portmap::start(&pm_cfg, lan).await;
                    crate::portmap::log_outcome(&session.outcome, pm_cfg.port);
                    portmap_hint = crate::portmap::hint_of(&session.outcome);
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
                    portmap_hint = crate::dialability::PortMapHint::Refused(
                        "no LAN IPv4 address on this host".to_string(),
                    );
                }
            }
        }
    }

    // ---- Identify carries the network identity (GPN #159) --------------
    // `identify`'s agent version is `<client_version> (<node_name>)`, fixed at
    // construction. Putting the chain id + genesis prefix in the node name means
    // even a peer that never opens a GPN protocol can see at the handshake that
    // it is talking to a different network — the human-visible half of #159.
    // The live height cannot live here (identify is not re-pushed per block), so
    // it travels in `gpn/chain/1` below, where it is always read fresh.
    {
        let info = client.chain_info();
        let registry = crate::network_registry::load_registry();
        let tag = format!(
            " [{}:{}]",
            registry.chain_id,
            short_hash(&info.genesis_hash)
        );
        if !config.network.node_name.ends_with(&tag) {
            config.network.node_name.push_str(&tag);
        }
    }

    let mut net_config = sc_network::config::FullNetworkConfiguration::<
        Block,
        <Block as sp_runtime::traits::Block>::Hash,
        N,
    >::new(&config.network, config.prometheus_registry().cloned());
    let metrics = N::register_notification_metrics(config.prometheus_registry());

    // ---- Peer-exchange notification protocol (GPN #155, design §2) --------
    // `gpn/peers/1` rides the existing connections, so it needs no dialling of
    // its own: the ranked re-dial loop below owns who we are connected to, and
    // this protocol only says who *else* exists. Registration must happen
    // before `build_network`; the protocol task is spawned after it.
    let peer_exchange_service: Box<dyn sc_network::NotificationService> = {
        let (protocol_config, service) = N::notification_config(
            sc_network::ProtocolName::from(crate::peer_exchange::PEERS_PROTOCOL),
            Vec::new(),
            crate::peer_exchange::MAX_GOSSIP_BYTES as u64,
            None,
            sc_network::config::SetConfig {
                // Gossip rides on whatever connections the node already has; a
                // bounded substream budget keeps a hostile peer from opening
                // an unbounded number of them.
                in_peers: 64,
                out_peers: 64,
                reserved_nodes: Vec::new(),
                non_reserved_mode: sc_network::config::NonReservedPeerMode::Accept,
            },
            metrics.clone(),
            net_config.peer_store_handle(),
        );
        net_config.add_notification_protocol(protocol_config);
        service
    };

    // ---- Chain-state announcement protocol (GPN #159) -------------------
    // Four facts on the connection itself: genesis + best hash @ height. A
    // peer on another network is named the moment the substream opens, and a
    // peer at our height with a different block is the observed fork shape —
    // both are visible without waiting for the next daily checkpoint refresh.
    let chain_state_service: Box<dyn sc_network::NotificationService> = {
        let (protocol_config, service) = N::notification_config(
            sc_network::ProtocolName::from(crate::chain_state::CHAIN_STATE_PROTOCOL),
            Vec::new(),
            crate::chain_state::MAX_BYTES as u64,
            None,
            sc_network::config::SetConfig {
                // One tiny message per peer per announce; a modest substream
                // budget is enough because we never stream and never keep a
                // long-lived send open.
                in_peers: 64,
                out_peers: 64,
                reserved_nodes: Vec::new(),
                non_reserved_mode: sc_network::config::NonReservedPeerMode::Accept,
            },
            metrics.clone(),
            net_config.peer_store_handle(),
        );
        net_config.add_notification_protocol(protocol_config);
        service
    };

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

    // ---- Connection sampler (2026-11-11) -------------------------------
    // The event stream in this SDK revision delivers nothing for connection
    // lifecycle, so poll the connected-peer count and log every CHANGE with a
    // timestamp. A remote box completes TCP and is closed within ~2 s; that
    // spike must be visible here if libp2p ever counted it.
    {
        let net = network.clone();
        task_manager
            .spawn_handle()
            .spawn("gpn-conn-sampler", None, async move {
                let mut last = usize::MAX;
                loop {
                    let n = net.sync_num_connected();
                    if n != last {
                        log::info!("conn: connected peers = {n}");
                        last = n;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            });
    }

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
    // Soft mode is the DEFAULT: a node that cannot be dialled prints the full
    // operator notice once (with the exact port-forward fix) and keeps syncing,
    // mining and serving as a non-discoverable participant, re-checking every
    // 10 min and printing NOW DIALABLE when the forward works (design §5.3).
    // `--require-backbone` / `GPN_REQUIRE_BACKBONE=1` is the opt-in legacy hard
    // gate that exits EXIT_NOT_DIALABLE instead.
    {
        if gate_cfg.require {
            if gate_cfg.site_gateway {
                log::info!(
                    "dialability: GPN_SITE_GATEWAY=1 — this machine is the advertised gateway \
                     for its LAN/egress (it must forward TCP and will publish a dialable address)"
                );
            }
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
            let state_source = verdict
                .evidence
                .as_ref()
                .map(|e| e.to_string())
                .unwrap_or_else(|| "none".into());

            // IPv6 publication is evidence-gated exactly like IPv4: configured
            // by the operator, observed by a peer, or an inbound connection
            // that arrived over v6. A detected interface address alone is a
            // candidate, not proof.
            let observed_now = network.external_addresses();
            let inbound_v6_now = network
                .network_state()
                .await
                .map(|s| crate::dialability::inbound_v6_peer_count(&s))
                .unwrap_or(0);
            let detected_addr = detected_v6.as_ref().and_then(|s| s.addr());
            let observed_addr = crate::dialability::observed_v6(&observed_now, seed_port);
            let v6_endpoint: Option<sc_network::multiaddr::Multiaddr> = if let Some(cfg) =
                configured_v6.clone()
            {
                Some(cfg)
            } else if let Some(o) = observed_addr {
                // Prefer the detected address when it is the one observed;
                // otherwise publish what a peer actually saw.
                let chosen = match detected_addr {
                    Some(d)
                        if crate::dialability::prefix64(&d) == crate::dialability::prefix64(&o) =>
                    {
                        d
                    }
                    _ => o,
                };
                crate::dialability::v6_base(&chosen, seed_port)
            } else if inbound_v6_now > 0 {
                detected_addr.and_then(|d| crate::dialability::v6_base(&d, seed_port))
            } else {
                None
            };

            let v4_endpoint = gate_cfg
                .public_addr
                .clone()
                .filter(|a| !crate::dialability::is_v6_endpoint(a));

            // Gateway role (SPEC 47 §5, design-lan-topology §3.1): one
            // verified public gateway per public IP.
            //
            // * `GPN_SITE_ELECT` off (default): exactly the old rule — the
            //   operator names the gateway with `GPN_SITE_GATEWAY`.
            // * on: the role requires a LIVE VERIFIED claim on our own socket,
            //   read from the verifier's claims file. Election only ranks
            //   verified claimants behind one public IP; it never grants
            //   ownership. Two boxes behind one NAT therefore cannot both be
            //   the gateway — the loser demotes on its own next cycle.
            let self_peer_id = network.local_peer_id().to_string();
            let self_socket = v4_endpoint.as_ref().and_then(|a| {
                crate::dialability::host_port_of(a).and_then(|(host, port)| match host {
                    crate::dialability::Host::Ip(ip) => Some(format!("{ip}:{port}")),
                    // A /dns name is resolved at check time; the claim names
                    // the resolved IP, so a name is not a socket here.
                    crate::dialability::Host::Dns(_) => None,
                })
            });
            let claims =
                crate::dialability::read_claims_file(&crate::dialability::claims_state_path());
            let now_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let role = crate::dialability::gateway_role(
                &gate_cfg,
                &self_peer_id,
                self_socket.as_deref(),
                &claims,
                &[],
                now_unix,
            );
            let gateway_flag = role.is_gateway();
            // With election off the gate's verdict is used verbatim (today's
            // behaviour). With election on an unverified node publishes
            // nothing, whatever its own evidence says.
            let dialable = if gate_cfg.site_elect {
                verdict.dialable && role.is_dialable()
            } else {
                verdict.dialable
            };
            if gate_cfg.site_elect {
                log::info!(
                    "dialability: site election → role={} (verified claims: {}, socket: {})",
                    role.as_str(),
                    claims.len(),
                    self_socket.as_deref().unwrap_or("none")
                );
            }

            // The endpoint set the heartbeat may publish *if* this node is a
            // backbone. `write_state_file` drops it for a leaf, so the tier
            // and the address can never disagree.
            let publication = crate::dialability::StatePublication {
                public: v4_endpoint.clone(),
                public6: v6_endpoint.clone(),
                source: state_source.clone(),
                peer_id: Some(self_peer_id.clone()),
                probe_peer_id: None,
            };

            // Publish the outcome for the provider heartbeat (SPEC 47: the
            // reachability tier is part of the verdict, not a separate flag).
            // The address is only written when the gate has EVIDENCE for it.
            crate::dialability::write_state_file(
                &publication,
                dialable,
                gateway_flag,
                &verdict.detail,
            );

            // Prefix rotation: ISPs renumber. Re-detect on the existing
            // 10-minute loop; on a change, drop the stale address and publish
            // the new one (once). The local libp2p announce set is fixed at
            // startup in this SDK, so the state file is the live channel.
            //
            // Only for auto-detected endpoints: an operator-configured `/ip6`
            // or `/dns6` endpoint is authoritative and must never be replaced
            // by an interface guess (a `/dns6` name re-resolves by itself).
            if gate_cfg.require && gate_cfg.discoverable && configured_v6.is_none() {
                let watch_net = network.clone();
                let watch_v4 = v4_endpoint.clone();
                let watch_initial = v6_endpoint
                    .as_ref()
                    .and_then(crate::dialability::ip6_of)
                    .or(detected_addr);
                // Continuity is only granted to an endpoint that was itself
                // proven over v6 at startup — either a peer reported it, or a
                // peer actually connected to us over v6. "The v4 gate passed"
                // is not v6 evidence (design §5).
                let trusted =
                    watch_initial.is_some() && (observed_addr.is_some() || inbound_v6_now > 0);
                let source = state_source.clone();
                let detail = verdict.detail.clone();
                // Captured for the `move` closure below; `dialable` and
                // `gateway_flag` were computed once above and must not change
                // with a later re-check of the v4 gate.
                let gateway = gateway_flag;
                let peer_id = self_peer_id.clone();
                task_manager
                    .spawn_handle()
                    .spawn("gpn-ipv6-prefix-watch", None, async move {
                        crate::dialability::watch_ipv6_prefix(
                            watch_v4,
                            watch_initial,
                            seed_port,
                            move |new| {
                                // The new address must be observed by a peer,
                                // or the node must already have a proven v6
                                // inbound path (same router, same pinhole).
                                trusted
                                    || crate::dialability::observed_v6(
                                        &watch_net.external_addresses(),
                                        seed_port,
                                    ) == Some(*new)
                            },
                            move |action| {
                                let v4 = action
                                    .bases
                                    .iter()
                                    .find(|a| !crate::dialability::is_v6_endpoint(a));
                                let v6 = action
                                    .bases
                                    .iter()
                                    .find(|a| crate::dialability::is_v6_endpoint(a));
                                crate::dialability::write_state_file(
                                    &crate::dialability::StatePublication {
                                        public: v4.cloned(),
                                        public6: v6.cloned(),
                                        source: source.clone(),
                                        peer_id: Some(peer_id.clone()),
                                        probe_peer_id: None,
                                    },
                                    dialable,
                                    gateway,
                                    &detail,
                                );
                            },
                        )
                        .await;
                    });
            }
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
                    &portmap_hint,
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
                    // Default path: refuse to run a node nobody can dial —
                    // UNLESS that refusal would take down the last authoring
                    // node (or a dev/solo chain). The gate protects the mesh;
                    // it must never halt the chain (2026-10-20 incident).
                    let safety_ctx = crate::dialability::SafetyContext {
                        is_authority: config.role.is_authority(),
                        connected_peers: network.sync_num_connected(),
                        authority_count: 0, // PoW chain: no on-chain authority set
                        dev_chain: is_dev_chain(&config),
                    };
                    let override_reason = if gate_cfg.safety {
                        crate::dialability::safety_valve(&safety_ctx)
                    } else {
                        None
                    };

                    if let Some(reason) = override_reason {
                        for line in
                            crate::dialability::safety_valve_notice(reason, &verdict.detail).lines()
                        {
                            log::warn!("{line}");
                        }
                        log::warn!(
                            "dialability: continuing in degraded mode [tier: {}] — the hard \
                             gate was overridden to keep the chain alive",
                            gate_cfg.intended_tier(false)
                        );
                        let watch_cfg = gate_cfg.clone();
                        let watch_net = network.clone();
                        let watch_pub = publication.clone();
                        task_manager.spawn_handle().spawn(
                            "gpn-dialability-watch",
                            None,
                            async move {
                                crate::dialability::watch_dialability(
                                    watch_net, watch_cfg, false, watch_pub,
                                )
                                .await;
                            },
                        );
                    } else if crate::dialability::hard_gate_must_exit(hard, false) {
                        log::error!("   reason: {}", verdict.detail);
                        // Distinct exit code so `gpn_node` can report
                        // `node=down(not dialable)` instead of a generic crash.
                        std::process::exit(crate::dialability::EXIT_NOT_DIALABLE);
                    }
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
                let watch_cfg = {
                    let mut c = gate_cfg.clone();
                    // The re-check loop republishes the same role; with election
                    // on, that role came from verified claims, not the operator
                    // flag.
                    c.site_gateway = gateway_flag;
                    c
                };
                let watch_net = network.clone();
                let start_dialable = dialable;
                let watch_pub = publication.clone();
                task_manager
                    .spawn_handle()
                    .spawn("gpn-dialability-watch", None, async move {
                        crate::dialability::watch_dialability(
                            watch_net,
                            watch_cfg,
                            start_dialable,
                            watch_pub,
                        )
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
            // No evidence means leaf. Publish that, so a stale backbone verdict
            // from a previous run cannot keep a non-probed node in seed lists.
            crate::dialability::write_state_file(
                &crate::dialability::StatePublication {
                    source: "probe-disabled".into(),
                    peer_id: Some(network.local_peer_id().to_string()),
                    ..Default::default()
                },
                false,
                gate_cfg.site_gateway,
                "dialability probe disabled by configuration",
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
        let base_for_watch = base_path.clone();
        task_manager
            .spawn_handle()
            .spawn("gpn-peer-watch", None, async move {
                let mut tick = tokio::time::interval(Duration::from_secs(30));
                tick.tick().await; // consume the immediate first tick
                loop {
                    tick.tick().await;
                    let isolated = net.sync_num_connected() == 0;
                    if isolated {
                        // 2026-11-09 (owner decision): a node that cannot see the
                        // network must not extend it. Box 1 spent days mining a
                        // 117k-block fork alone -- six healthy children, a green
                        // dashboard, and a chain nobody else had. "Identity is
                        // genesis + chain-id" is still true, but extending a
                        // private chain is indistinguishable from working until
                        // somebody compares heights, so sealing pauses instead.
                        let _ = std::fs::write(
                            base_for_watch.join("gpn-isolated"),
                            b"no peers: block sealing paused\n",
                        );
                        log::warn!(
                            "⚠️ No peers — block sealing is PAUSED. This node is not \
                             connected to the GPN network; it will resume mining as soon as a \
                             peer connects. (Marker: <base-path>/gpn-isolated)"
                        );
                    } else {
                        let _ = std::fs::remove_file(base_for_watch.join("gpn-isolated"));
                    }
                }
            });
    }

    // ---- Fork tolerance (#158): divergence is not misbehaviour -----------
    // A peer whose best block differs from ours at the SAME height has done
    // nothing wrong: it is on a side branch (behind, or ahead on a fork it can
    // reorg off) and the cure is to stay connected and keep serving canonical
    // blocks, not to drop it. `sc-network-sync` cannot always tell divergence
    // from a protocol violation -- a block that fails verification is reported
    // as `BAD_BLOCK`, and about three of those ban a peer -- so this task owns
    // the local repair: on a detected divergence it credits reputation back, at
    // most once per window per peer, never above zero (module `fork_tolerance`).
    // It never disconnects and never changes chain selection.
    {
        let sync = sync_service.clone();
        let net = network.clone();
        let client = client.clone();
        task_manager
            .spawn_handle()
            .spawn("gpn-fork-tolerance", None, async move {
                use crate::fork_tolerance::{self, Verdict};
                let mut guard = fork_tolerance::DivergenceGuard::default();
                let start = std::time::Instant::now();
                let mut tick =
                    tokio::time::interval(Duration::from_secs(fork_tolerance::SCAN_INTERVAL_SECS));
                tick.tick().await; // consume the immediate first tick
                loop {
                    tick.tick().await;
                    let ours = client.chain_info();
                    let Ok(peers) = sync.peers_info().await else {
                        continue;
                    };
                    let now_secs = start.elapsed().as_secs();
                    for (peer_id, info) in peers {
                        if fork_tolerance::classify(
                            ours.best_number,
                            &ours.best_hash,
                            info.best_number,
                            &info.best_hash,
                        ) != Verdict::Diverged
                        {
                            continue;
                        }
                        let key = peer_id.to_string();
                        if !guard.should_repair(&key, now_secs) {
                            continue;
                        }
                        let current = net.peer_reputation(&peer_id);
                        let credit = fork_tolerance::divergence_credit(current);
                        if credit > 0 {
                            net.report_peer(
                                peer_id,
                                ReputationChange::new(
                                    credit,
                                    "chain divergence is not misbehaviour (#158)",
                                ),
                            );
                        }
                        log::info!(
                            "fork-tolerance: peer {} is on a different chain at #{} (ours {}, \
                             theirs {}) -- keeping the link and serving canonical blocks; \
                             reputation {} -> {} (divergence is not a protocol violation)",
                            key,
                            ours.best_number,
                            crate::service::short_hash(&ours.best_hash),
                            crate::service::short_hash(&info.best_hash),
                            current,
                            current.saturating_add(credit)
                        );
                    }
                }
            });
    }

    // ---- Ranked peer book (GPN #154, design §1) --------------------------
    // Every discovery source writes here; the ranked re-dial loop below is the
    // single place that decides dial order, slot usage and backoff.
    let peer_book = std::sync::Arc::new(crate::peer_priority::PeerBook::new());

    // Consecutive dial failures per peer, shared with the gossip policy so an
    // address that has stopped answering stops being advertised (design §2).
    let dial_failures: std::sync::Arc<Mutex<std::collections::HashMap<String, u32>>> =
        Default::default();

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
    //
    // Since GPN #154 the endpoints are fed into the ranked peer book (tier 1,
    // "on chain") and dialled by the ranked re-dial loop below — order and cap
    // are decided in one place. Chain records carry no first-seen block, so
    // tenure is unknown and peers tie-break on peer id (see `peer_priority`).
    {
        let net = network.clone();
        let client = client.clone();
        let book = peer_book.clone();
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
                            book.upsert(crate::peer_priority::Candidate::new(
                                &ma,
                                crate::peer_priority::Tier::Ranked,
                                None,
                            ));
                            added += 1;
                        }
                    }
                    if added > 0 {
                        log::info!(
                            "chain-discovery: {added} on-chain peer(s) added to the ranked list"
                        );
                    } else {
                        log::debug!("chain-discovery: no dialable on-chain endpoints");
                    }
                }
            });
    }

    // ---- Periodic seed-list refresh (design §3.3) ------------------------
    // Startup resolution is a snapshot: a publisher that adds seeds, a DNS
    // seed that changes, or a long-running node that drifts down to zero peers
    // would otherwise never re-read it. Refresh at 15 min while short of
    // peers / 1 h otherwise, with ±15 % jitter and a backoff multiplier on
    // consecutive failures. Since GPN #154 the results go into the ranked peer
    // book (tier 0 only for a compiled-in oracle seed, otherwise tier 2) and
    // are dialled by the ranked re-dial loop — the shuffle is gone, because
    // two nodes shuffling differently is exactly how the fleet split into two
    // pairs on 2026-11-09.
    if let Some(sources) = crate::discovery::active_sources() {
        if !(sources.dns.is_empty() && sources.urls.is_empty()) {
            let net = network.clone();
            let base = base_path.clone();
            let sources = sources.clone();
            let book = peer_book.clone();
            let oracle_ids = crate::peer_priority::oracle_peer_ids(
                &crate::network_registry::load_registry().bootstrap_seeds,
            );
            task_manager
                .spawn_handle()
                .spawn("gpn-seed-refresh", None, async move {
                    let mut failures: u32 = 0;
                    let fast = crate::discovery::fast_refresh_secs();
                    loop {
                        let connected = net.sync_num_connected();
                        let sample = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.subsec_nanos() as u64)
                            .unwrap_or(0);
                        let jitter = (sample / 10) % 100;
                        let wait = crate::discovery::refresh_interval_secs_with(
                            connected, failures, jitter, fast,
                        );
                        tokio::time::sleep(Duration::from_secs(wait)).await;

                        match crate::discovery::fetch_all_seeds(&sources).await {
                            Ok(addrs) => {
                                failures = 0;
                                if let Err(e) = crate::discovery::write_seed_cache(&base, &addrs) {
                                    log::debug!("seed-refresh: cache write failed: {e}");
                                }
                                let mut known = 0usize;
                                for ma in addrs {
                                    let tier = match crate::peer_priority::peer_id_of(&ma) {
                                        Some(id) if oracle_ids.iter().any(|o| o == id) => {
                                            crate::peer_priority::Tier::OracleSeed
                                        }
                                        Some(_) => crate::peer_priority::Tier::Unknown,
                                        None => continue,
                                    };
                                    book.upsert(crate::peer_priority::Candidate::new(
                                        &ma, tier, None,
                                    ));
                                    known += 1;
                                }
                                log::info!(
                                    "seed-refresh: {known} seed(s) re-read from {} source(s) \
                                     into the ranked list (peers {connected}); next refresh in {}s",
                                    sources.dns.len() + sources.urls.len(),
                                    crate::discovery::refresh_interval_secs_with(
                                        connected, 0, jitter, fast
                                    )
                                );
                            }
                            Err(e) => {
                                failures = failures.saturating_add(1);
                                // Quiet while retrying: a down publisher is
                                // expected and must not spam the operator.
                                log::debug!(
                                    "seed-refresh: source(s) unavailable ({e}); \
                                     attempt {failures}, backing off"
                                );
                            }
                        }
                    }
                });
        }
    }

    // ---- Ranked peer re-dial (GPN #154, design §1) -----------------------
    // The one place that decides who to dial and in what order:
    //   * tier 0 — oracle seeds: every 60 s while disconnected, always dialled,
    //     never capped and never evicted (also reserved in the network config);
    //   * tier 1 — on-chain peers, ranked by tenure (unknown tenure last);
    //   * tier 2 — unknown peers, only while non-reserved slots remain.
    // A failed dial backs off 30 s → 8 min (capped) but never removes the peer,
    // because the peer that is down today is the peer that holds the history
    // tomorrow. Connectivity comes from `network_state()`, so a connected peer
    // is never re-dialled and a successful one resets its backoff.
    {
        let net = network.clone();
        let book = peer_book.clone();
        let base = base_path.clone();
        let shared_failures = dial_failures.clone();
        let oracle_ids = crate::peer_priority::oracle_peer_ids(
            &crate::network_registry::load_registry().bootstrap_seeds,
        );
        task_manager
            .spawn_handle()
            .spawn("gpn-peer-redial", None, async move {
                use crate::peer_priority::{self, Tier};
                let max_peers = peer_priority::max_peers_from_env();
                log::info!(
                    "peer-priority: ranked re-dial active — {max_peers} non-reserved slot(s), \
                     {} oracle seed(s), {}s tick",
                    oracle_ids.len(),
                    peer_priority::REDIAL_INTERVAL_SECS / 2
                );
                let mut last_attempt: std::collections::HashMap<String, u64> = Default::default();
                let mut failures: std::collections::HashMap<String, u32> = Default::default();
                // Seed the book with what this node already knows, so the very
                // first tick can rank and dial without waiting for a refresh:
                // compiled-in bootnodes, the persisted peer store and the
                // last-good seed cache.
                let mut known: Vec<crate::peer_priority::Candidate> = Vec::new();
                for ma in crate::network_registry::load_registry().bootnodes {
                    let tier = match crate::peer_priority::peer_id_of(&ma) {
                        Some(id) if oracle_ids.iter().any(|o| o == id) => Tier::OracleSeed,
                        Some(_) => Tier::Unknown,
                        None => continue,
                    };
                    known.push(crate::peer_priority::Candidate::new(&ma, tier, None));
                }
                for ma in crate::discovery::read_persisted_peers(&base)
                    .into_iter()
                    .chain(crate::discovery::read_seed_cache(
                        &base,
                        crate::discovery::SEED_CACHE_MAX_AGE_SECS,
                    ))
                {
                    known.push(crate::peer_priority::Candidate::new(
                        &ma,
                        Tier::Unknown,
                        None,
                    ));
                }
                book.upsert_all(known);
                log::debug!("peer-priority: book seeded with {} peer(s)", book.len());
                let mut tick = tokio::time::interval(Duration::from_secs(
                    peer_priority::REDIAL_INTERVAL_SECS / 2,
                ));
                loop {
                    tick.tick().await;
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let connected: std::collections::HashSet<String> =
                        match net.network_state().await {
                            Ok(state) => state
                                .connected_peers
                                .keys()
                                .map(|p| p.to_string())
                                .collect(),
                            Err(_) => Default::default(),
                        };
                    // Tier-0 is defined by the compiled-in seed set, not by
                    // whoever wrote the entry into the book.
                    let mut candidates = book.snapshot();
                    for c in candidates.iter_mut() {
                        if oracle_ids.iter().any(|o| o == &c.peer_id) {
                            c.tier = Tier::OracleSeed;
                        }
                    }
                    if candidates.is_empty() {
                        continue;
                    }
                    let plan = peer_priority::dial_plan(&candidates, max_peers);
                    let mut dialled = 0usize;
                    for c in &plan.dial {
                        if connected.contains(&c.peer_id) {
                            last_attempt.insert(c.peer_id.clone(), now);
                            failures.insert(c.peer_id.clone(), 0);
                            continue;
                        }
                        let since = last_attempt
                            .get(&c.peer_id)
                            .map(|t| now.saturating_sub(*t))
                            .unwrap_or(u64::MAX);
                        let f = *failures.get(&c.peer_id).unwrap_or(&0);
                        if !peer_priority::due_for_redial(c.tier, since, f) {
                            continue;
                        }
                        if let Ok(mw) = c.addr.parse::<MultiaddrWithPeerId>() {
                            net.add_known_address(mw.peer_id, mw.multiaddr);
                            dialled += 1;
                        }
                        last_attempt.insert(c.peer_id.clone(), now);
                        failures.insert(c.peer_id.clone(), f.saturating_add(1).min(8));
                    }
                    // Publish the failure record for the gossip policy. A peer
                    // that has failed three dials in a row is not re-advertised.
                    if let Ok(mut shared) = shared_failures.lock() {
                        *shared = failures.clone();
                    }
                    if dialled > 0 {
                        let reserved = plan
                            .dial
                            .iter()
                            .filter(|c| c.tier == Tier::OracleSeed)
                            .count();
                        log::info!(
                            "peer-priority: dialled {dialled}/{} ranked peer(s) — {reserved} \
                             reserved, {} on-chain/unknown, {} with no slot this round; \
                             {}/{max_peers} slot(s) filled",
                            plan.dial.len(),
                            plan.dial.len() - reserved,
                            plan.skipped.len(),
                            plan.dial.len()
                        );
                    }
                }
            });
    }

    // ---- Peer-exchange gossip (GPN #155, design §2) ----------------------
    // A node that only knows the seed pairs off with whoever dials it — that is
    // exactly how the fleet became two disjoint pairs on 2026-11-09. Every 5 min
    // we tell each connected peer who else we know (≤32 entries, ≤8 KB), and
    // anything we learn enters the ranked book as tier 2, so gossip can never
    // displace a peer the chain vouched for. Best effort by construction: a
    // malformed message yields nothing, never an error the node acts on.
    {
        let book = peer_book.clone();
        let failures = dial_failures.clone();
        task_manager
            .spawn_handle()
            .spawn("gpn-peer-exchange", None, async move {
                use crate::peer_exchange as px;
                let mut service = peer_exchange_service;
                let mut open: std::collections::HashSet<sc_network::PeerId> = Default::default();
                let mut learned: std::collections::HashMap<String, u64> = Default::default();
                let mut ticker =
                    tokio::time::interval(Duration::from_secs(px::GOSSIP_INTERVAL_SECS));
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                log::info!(
                    "peer-exchange: {} active — ≤{} entries / {} B every {}s",
                    px::PEERS_PROTOCOL,
                    px::MAX_GOSSIP_ENTRIES,
                    px::MAX_GOSSIP_BYTES,
                    px::GOSSIP_INTERVAL_SECS
                );
                loop {
                    tokio::select! {
                        event = service.next_event() => {
                            let Some(event) = event else {
                                log::warn!(
                                    "peer-exchange: notification stream ended — gossip stopped"
                                );
                                break;
                            };
                            match event {
                                NotificationEvent::ValidateInboundSubstream {
                                    result_tx, handshake, ..
                                } => {
                                    // Our handshake is empty; a peer sending a
                                    // large one is not speaking this protocol.
                                    let _ = result_tx.send(if handshake.len() <= 64 {
                                        ValidationResult::Accept
                                    } else {
                                        ValidationResult::Reject
                                    });
                                }
                                NotificationEvent::NotificationStreamOpened { peer, .. } => {
                                    open.insert(peer);
                                    // INFO on purpose (2026-11-11): the handshake
                                    // outcome is otherwise invisible. A peer that
                                    // reaches this line completed Noise + multistream;
                                    // a peer that never appears here died in the
                                    // transport upgrade. Release builds compile out
                                    // DEBUG, so this must be INFO to be observable.
                                    log::info!(
                                        "peer-exchange: substream open with {peer} ({} peer(s))",
                                        open.len()
                                    );
                                }
                                NotificationEvent::NotificationStreamClosed { peer } => {
                                    if open.remove(&peer) {
                                        log::info!(
                                            "peer-exchange: substream closed with {peer} ({} peer(s))",
                                            open.len()
                                        );
                                    }
                                }
                                NotificationEvent::NotificationReceived {
                                    peer, notification, ..
                                } => {
                                    let now = now_unix();
                                    let before = book.len();
                                    let added = px::ingest(
                                        &notification,
                                        &book,
                                        before,
                                        &mut learned,
                                        now,
                                    );
                                    if added > 0 {
                                        log::info!(
                                            "peer-exchange: learned {added} new peer(s) from \
                                             {peer} ({} known total)",
                                            book.len()
                                        );
                                    }
                                }
                            }
                        }
                        _ = ticker.tick() => {
                            if open.is_empty() {
                                continue;
                            }
                            let now = now_unix();
                            let candidates = book.snapshot();
                            let fail = failures
                                .lock()
                                .map(|g| g.clone())
                                .unwrap_or_default();
                            let mut sent = 0usize;
                            for peer in &open {
                                let who = peer.to_string();
                                let entries = px::select_entries(
                                    &candidates,
                                    &learned,
                                    &fail,
                                    Some(who.as_str()),
                                    now,
                                );
                                if entries.is_empty() {
                                    continue;
                                }
                                let bytes = px::encode(&entries, now);
                                if bytes.is_empty() {
                                    continue;
                                }
                                service.send_sync_notification(peer, bytes);
                                sent += 1;
                            }
                            if sent > 0 {
                                log::debug!(
                                    "peer-exchange: gossiped peer list to {sent}/{} peer(s)",
                                    open.len()
                                );
                            }
                        }
                    }
                }
            });
    }

    // ---- Chain-state announcement (GPN #159) -----------------------------
    // Sent to a peer the moment its substream opens, then again only when our
    // best height actually advanced: a node that is idle is silent, and a node
    // that is syncing is never more than `chain_state::TICK_SECS` stale. The
    // comparison is edge-triggered per peer (`StateLog`), so a peer parked on a
    // fork produces one loud line, not one per tick.
    {
        let client = client.clone();
        task_manager
            .spawn_handle()
            .spawn("gpn-chain-state", None, async move {
                use crate::chain_state as cs;
                let mut service = chain_state_service;
                let mut open: std::collections::HashSet<sc_network::PeerId> = Default::default();
                let mut announced: std::collections::HashMap<sc_network::PeerId, u64> =
                    Default::default();
                let mut log_state = cs::StateLog::default();
                let mut ticker = tokio::time::interval(Duration::from_secs(cs::TICK_SECS));
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                log::info!(
                    "chain-state: {} active — genesis:hash@height on connect and on every \
                     tip advance ({}s tick)",
                    cs::CHAIN_STATE_PROTOCOL,
                    cs::TICK_SECS
                );
                loop {
                    tokio::select! {
                        event = service.next_event() => {
                            let Some(event) = event else {
                                log::warn!(
                                    "chain-state: notification stream ended — announcements stopped"
                                );
                                break;
                            };
                            match event {
                                NotificationEvent::ValidateInboundSubstream {
                                    result_tx, handshake, ..
                                } => {
                                    // Our handshake is empty; a peer sending a
                                    // large one is not speaking this protocol.
                                    let _ = result_tx.send(if handshake.len() <= 64 {
                                        ValidationResult::Accept
                                    } else {
                                        ValidationResult::Reject
                                    });
                                }
                                NotificationEvent::NotificationStreamOpened { peer, .. } => {
                                    open.insert(peer);
                                    // Announce immediately: this is the whole
                                    // point — no waiting for a checkpoint.
                                    if let Some(local) = crate::service::local_chain_state(&client) {
                                        if let Some(bytes) = cs::encode(&local) {
                                            service.send_sync_notification(&peer, bytes);
                                            announced.insert(peer, local.height);
                                        }
                                    }
                                    log::debug!(
                                        "chain-state: substream open with {peer} ({} peer(s))",
                                        open.len()
                                    );
                                }
                                NotificationEvent::NotificationStreamClosed { peer } => {
                                    open.remove(&peer);
                                    announced.remove(&peer);
                                    log_state.forget(&peer.to_string());
                                }
                                NotificationEvent::NotificationReceived {
                                    peer, notification, ..
                                } => {
                                    let Some(remote) = cs::decode(&notification) else {
                                        log::debug!(
                                            "chain-state: ignored an undecodable claim from {peer}"
                                        );
                                        continue;
                                    };
                                    // Read our own tip fresh: an unreadable tip
                                    // publishes nothing (never a stale fact).
                                    let Some(local) = crate::service::local_chain_state(&client) else {
                                        continue;
                                    };
                                    let verdict = cs::compare(&local, &remote);
                                    let who = peer.to_string();
                                    if log_state.record(&who, verdict, &remote).is_some() {
                                        let line = cs::describe(&who, &local, &remote, verdict);
                                        if verdict.is_notable() {
                                            log::warn!("{line}");
                                        } else {
                                            log::info!("{line}");
                                        }
                                    }
                                }
                            }
                        }
                        _ = ticker.tick() => {
                            if open.is_empty() {
                                continue;
                            }
                            let Some(local) = crate::service::local_chain_state(&client) else {
                                continue;
                            };
                            let Some(bytes) = cs::encode(&local) else {
                                continue;
                            };
                            let mut sent = 0usize;
                            for peer in &open {
                                if !cs::announce_due(
                                    announced.get(peer).copied(),
                                    local.height,
                                ) {
                                    continue;
                                }
                                service.send_sync_notification(peer, bytes.clone());
                                announced.insert(*peer, local.height);
                                sent += 1;
                            }
                            if sent > 0 {
                                log::debug!(
                                    "chain-state: announced #{} to {sent}/{} peer(s)",
                                    local.height,
                                    open.len()
                                );
                            }
                        }
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
            // Idle survives a restart: the miner thread still starts (it is what
            // resumes at the deadline) but parks immediately and says so loudly,
            // so "why is my node not sealed for an hour?" is answered in the log.
            let idled = idle_requested();
            if !mining_allowed(role, idled) {
                log::warn!(
                    "⏸  IDLE MODE is in force ({}) — the miner starts PARKED: no nonce search. \
                     The node keeps syncing/peering and will resume mining automatically at the \
                     deadline (or on `gpn_node resume`).",
                    idle_file_path().display()
                );
            }
            spawn_miner(worker, author, base_path.clone());
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

// ─────────────────────────────────────────────────────────────────────────────
// Idle mode (the miner half)
// ─────────────────────────────────────────────────────────────────────────────
//
// Idle is the operator's "let me play a game" switch: **mining and AI job
// claiming pause, the node keeps syncing/peering/serving the chain**. The
// supervisor (`gpn_node`) and the providers are separate processes, so the
// contract is a file — `$GPN_IDLE_FILE`, else `~/.gpn/idle.json`, written by the
// GUI / `gpn_node idle` and read here on every miner iteration (the same
// mechanism as the GPU-yield flag above).
//
// Semantics fixed by the file format: `{"until": <unix>, "reason": ...,
// "started": <unix>}`. A file whose `until` is in the past is **expired** — it
// must read as "active", never as "idle forever", so a node that was off past
// the deadline starts mining again.

/// Path of the idle state file (env-aware, matching `gpn_agent::idle`).
fn idle_file_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("GPN_IDLE_FILE") {
        if !p.trim().is_empty() {
            return std::path::PathBuf::from(p);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::Path::new(&home).join(".gpn").join("idle.json")
}

/// Pure deadline test — `until > now` means idle is in force.
///
/// The boundary is inclusive of the deadline: **at** `until` the node is active
/// again ("resume at 18:00" means 18:00 is mining).
pub(crate) fn idle_active(until: Option<u64>, now: u64) -> bool {
    matches!(until, Some(u) if now < u)
}

/// Unix seconds now (0 before the epoch — never panic in a miner loop).
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A missing/corrupt/half-written state file must never park the miner, so any
/// parse problem is treated as "active".
fn read_idle_until(path: &std::path::Path) -> Option<u64> {
    let raw = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.get("until")?.as_u64()
}

/// True while the operator has the node idled.
fn idle_requested() -> bool {
    let path = idle_file_path();
    idle_active(read_idle_until(&path), now_unix())
}

/// Why the miner is parked this iteration (`None` = mine).
///
/// One GPU = one thing at a time: an AI job (yield flag) and an operator idle
/// window both mean "search no nonces", but they are reported separately so the
/// log says which one it is.
fn park_reason() -> Option<&'static str> {
    if idle_requested() {
        Some("idle")
    } else if gpu_yield_requested() {
        Some("ai-job")
    } else {
        None
    }
}

/// The miner's gate: role **and** the runtime idle state.
///
/// [`miner_should_run`] stays a pure role predicate because the miner thread must
/// be spawned even when the node starts idle (that thread is what resumes at the
/// deadline); this is the "may we search nonces right now" question.
pub(crate) fn mining_allowed(role: sc_service::Role, idle: bool) -> bool {
    miner_should_run(role) && !idle
}

/// How long the consensus worker may hand the miner **no template** before we
/// say so out loud.
///
/// `worker.metadata()` is `None` while `sc-consensus-pow` has no fresh proposal
/// for us — normal for a few hundred ms after every import. A *long* spell of it
/// was the one authoring stall the miner loop reported **nothing** about: it just
/// slept 100 ms and retried. That made a 97 s gap unattributable in the journal —
/// no seal line (we were not solving), no park line (we were not idled) and no
/// warning either, so the debug crons could only *infer* a cause (see #52/#54).
/// 30 s is comfortably above a normal inter-block gap at the 15 s target and well
/// below the 60 s+ tail that clients feel.
const TEMPLATE_GAP_WARN_MS: u128 = 30_000;

/// One line to emit about a template gap: `true` = warn (a stall), `false` = info
/// (it recovered).
struct GapLine {
    warn: bool,
    text: String,
}

/// Edge-triggered template-gap tracker: one WARN when the miner has had no work
/// for `TEMPLATE_GAP_WARN_MS`, one INFO when a template arrives again, silent in
/// between (the loop runs many times a second).
struct TemplateGap {
    /// Millisecond timestamp of the first iteration without a template.
    since_ms: Option<u128>,
    warned: bool,
}

impl TemplateGap {
    fn new() -> Self {
        Self {
            since_ms: None,
            warned: false,
        }
    }

    /// Feed one loop iteration.
    ///
    /// `has_template` is `worker.metadata().is_some()`; `now_ms` is milliseconds
    /// since the miner thread started (monotonic, so a backwards wall clock can
    /// never fabricate a gap).
    fn update(&mut self, has_template: bool, now_ms: u128) -> Option<GapLine> {
        if has_template {
            let started = self.since_ms.take()?;
            if !self.warned {
                // A short wait between blocks is normal — say nothing.
                return None;
            }
            self.warned = false;
            let secs = now_ms.saturating_sub(started) as f64 / 1000.0;
            return Some(GapLine {
                warn: false,
                text: format!(
                    "miner: consensus template available again after {secs:.1}s without work"
                ),
            });
        }

        let started = *self.since_ms.get_or_insert(now_ms);
        if self.warned {
            return None;
        }
        let waited = now_ms.saturating_sub(started);
        if waited < TEMPLATE_GAP_WARN_MS {
            return None;
        }
        self.warned = true;
        let secs = waited as f64 / 1000.0;
        Some(GapLine {
            warn: true,
            text: format!(
                "miner has had NO consensus template for {secs:.1}s (not parked, not solving): \
                 the authoring gap is on the proposer/consensus side, not the sealer"
            ),
        })
    }
}

/// Emit a [`GapLine`] at the level it asks for.
fn log_gap_line(line: GapLine) {
    if line.warn {
        log::warn!("{}", line.text);
    } else {
        log::info!("{}", line.text);
    }
}

/// Emits one log line per parking transition (never one per 500 ms poll).
struct ParkLog {
    state: Option<&'static str>,
}

impl ParkLog {
    fn new() -> Self {
        Self { state: None }
    }

    /// Returns the line to log for this transition, if any.
    fn update(&mut self, reason: Option<&'static str>) -> Option<String> {
        if reason == self.state {
            return None;
        }
        let previous = self.state;
        self.state = reason;
        Some(match (previous, reason) {
            (Some("idle"), None) => "idle ended, resuming mining".to_string(),
            (Some("idle"), Some(_)) => {
                "idle ended, resuming — the GPU goes to an ai-job first".to_string()
            }
            (_, Some("idle")) => "\u{23f8}  idle mode: miner parked (no nonce search); the node keeps syncing, peering and serving the chain".to_string(),
            (_, Some("ai-job")) => "miner parked: GPU yielded to an AI job".to_string(),
            (_, None) => "miner resuming: GPU is free again".to_string(),
            (_, Some(_)) => "miner parked: GPU unavailable".to_string(),
        })
    }
}

/// Spawn a dedicated miner thread: poll `worker.metadata()`, run real Ethash
/// mining to find a valid nonce + mix_hash, then submit the seal. Runs on its
/// own OS thread because the submit future holds a non-`Send` mutex guard
/// across an await.
///
/// Tries the **GPU OpenCL miner first** (full dataset on the GPU). If no
/// OpenCL GPU is available, falls back to the light-cache CPU miner so the
/// node still produces blocks on machines without a GPU.
fn spawn_miner(worker: MiningWorker, author: Option<[u8; 32]>, base_path: std::path::PathBuf) {
    log::info!(
        "⛏  miner start: probing GPU (device 0); author={} — will seal every block the proposer builds",
        match author {
            Some(_) => "set",
            None => "none (runtime BlockRewardBeneficiary fallback)",
        }
    );
    match EthashGpuMiner::new(0) {
        Ok(_) => spawn_gpu_miner(worker, author, base_path),
        Err(e) => {
            log::warn!("GPU miner unavailable ({e}); falling back to CPU light-cache miner");
            spawn_cpu_miner(worker, author, base_path);
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
fn spawn_gpu_miner(worker: MiningWorker, author: Option<[u8; 32]>, base_path: std::path::PathBuf) {
    std::thread::Builder::new()
        .name("gpn-gpu-miner".into())
        .spawn(move || {
            let miner = match EthashGpuMiner::new(0) {
                Ok(m) => m,
                Err(e) => {
                    // Never leave the proposer without a miner: fall back to CPU.
                    log::error!("GPU miner init failed: {e}; falling back to CPU light-cache miner");
                    spawn_cpu_miner(worker, author, base_path.clone());
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
                    spawn_cpu_miner(worker, author, base_path.clone());
                    return;
                }
            };

            let mut last_pre_hash: Option<<Block as BlockT>::Hash> = None;
            let mut nonce: u64 = 0;
            let mut park = ParkLog::new();
            let started_at = std::time::Instant::now();
            let mut gap = TemplateGap::new();
            loop {
                // Full stop when the GPU belongs to something else: an AI job
                // (the provider holds the yield flag) or an operator idle window
                // (idle.json — mining AND job claiming paused, the node still
                // syncs/peers/serves). No nonce search in either case; the thread
                // stays alive and parked, so resume is immediate with no
                // process/thread restart. One GPU = one thing at a time.
                let reason = park_reason();
                if let Some(line) = park.update(reason) {
                    log::info!("{line}");
                }
                if reason.is_some() {
                    std::thread::sleep(Duration::from_millis(GPU_YIELD_SLEEP_MS));
                    continue;
                }
                // Attribute a long absence of work instead of leaving it silent.
                let metadata = worker.metadata();
                if let Some(line) = gap.update(metadata.is_some(), started_at.elapsed().as_millis())
                {
                    log_gap_line(line);
                }
                if let Some(metadata) = metadata {
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
                            let sealing_nonce = seal.nonce;
                            // Another authority may have imported a block on this same
                            // parent while we were solving. `submit()` verifies the seal
                            // against the *current* template's `pre_hash`, so a solution
                            // that lost that race comes back as "seal is invalid" — an
                            // alarming message for a perfectly healthy miner (three debug
                            // crons mis-attributed ~42 s block gaps to it). Drop the
                            // doomed submission and re-read the template instead.
                            if !seal_still_wanted(&pre_hash, worker.metadata().as_ref().map(|m| &m.pre_hash)) {
                                log::debug!(
                                    "superseded while solving (pre_hash={pre_hash:?}, nonce={sealing_nonce}); restarting on the current template"
                                );
                                last_pre_hash = None;
                                continue;
                            }
                            // Owner decision 2026-11-09: never extend a chain we
                            // cannot see the network on. The peer-watch task writes
                            // <base-path>/gpn-isolated when connected peers hit 0;
                            // dropping the seal keeps the work spent (the DAG stays
                            // warm) without publishing a block into a private fork.
                            if base_path.join("gpn-isolated").exists() {
                                log::debug!("isolated (0 peers): not submitting this seal");
                                last_pre_hash = None;
                                std::thread::sleep(std::time::Duration::from_secs(5));
                                continue;
                            }
                            let seal_bytes = seal_with_author(seal.to_seal_bytes(), author);
                            let ok = futures::executor::block_on(worker.submit(seal_bytes));
                            if ok {
                                log::info!(
                                    "⛏  GPU sealed block: nonce={sealing_nonce} pre_hash={pre_hash:?}"
                                );
                            } else if seal_still_wanted(
                                &pre_hash,
                                worker.metadata().as_ref().map(|m| &m.pre_hash),
                            ) {
                                log::warn!(
                                    "GPU seal for pre_hash={pre_hash:?} (nonce={sealing_nonce}) was rejected by the node; retrying"
                                );
                            } else {
                                log::debug!(
                                    "seal for pre_hash={pre_hash:?} (nonce={sealing_nonce}) arrived after the template moved on; restarting"
                                );
                                last_pre_hash = None;
                                continue;
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
fn spawn_cpu_miner(worker: MiningWorker, author: Option<[u8; 32]>, _base_path: std::path::PathBuf) {
    std::thread::Builder::new()
        .name("gpn-cpu-miner".into())
        .spawn(move || {
            let cache = ethash_cache();
            let full_size = ethash_full_size();
            log::info!("CPU light-cache miner online (epoch {DEV_EPOCH}); no GPU backend");
            let mut nonce: u64 = 0;
            let mut park = ParkLog::new();
            let started_at = std::time::Instant::now();
            let mut gap = TemplateGap::new();
            loop {
                // Full stop (same contract as the GPU miner): an AI job's yield
                // flag or an operator idle window means we search no nonces at
                // all, then resume the instant it clears. Thread stays alive.
                let reason = park_reason();
                if let Some(line) = park.update(reason) {
                    log::info!("{line}");
                }
                if reason.is_some() {
                    std::thread::sleep(Duration::from_millis(GPU_YIELD_SLEEP_MS));
                    continue;
                }
                // Same gap attribution as the GPU path.
                let metadata = worker.metadata();
                if let Some(line) = gap.update(metadata.is_some(), started_at.elapsed().as_millis())
                {
                    log_gap_line(line);
                }
                if let Some(metadata) = metadata {
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
                            // Same race as the GPU path: a solution for a template a peer
                            // already built on is reported by `submit()` as "seal is
                            // invalid". Skip it rather than alarming the operator.
                            if !seal_still_wanted(
                                &pre_hash,
                                worker.metadata().as_ref().map(|m| &m.pre_hash),
                            ) {
                                log::debug!(
                                    "superseded while solving (pre_hash={pre_hash:?}, nonce={nonce_found}); restarting on the current template"
                                );
                                break;
                            }
                            let ok = futures::executor::block_on(worker.submit(seal));
                            if ok {
                                log::info!(
                                    "⛏  CPU sealed block: nonce={nonce_found} pre_hash={pre_hash:?}"
                                );
                            } else if seal_still_wanted(
                                &pre_hash,
                                worker.metadata().as_ref().map(|m| &m.pre_hash),
                            ) {
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

    /// Losing a mining race must be recognised as such: a seal is submitted only
    /// while the template it was solved for is still the live one. This is what
    /// keeps a healthy miner from logging `seal is invalid` (and paying a doomed
    /// verify+import) when a peer authority's block lands first.
    #[test]
    fn a_seal_is_only_submitted_for_the_live_template() {
        type Hash = <Block as BlockT>::Hash;
        let ours: Hash = [0x11u8; 32].into();
        let theirs: Hash = [0x22u8; 32].into();

        assert!(
            seal_still_wanted(&ours, Some(&ours)),
            "the template we solved for is still live -- submit"
        );
        assert!(
            !seal_still_wanted(&ours, Some(&theirs)),
            "a peer's block replaced the template -- the seal is dead, drop it"
        );
        assert!(
            !seal_still_wanted(&ours, None),
            "no live template (consumed by submit) -- nothing to submit against"
        );
    }

    /// The minimum-version cutoff, node side: `GPN_STANDBY=1` must downgrade an
    /// authority to a full node so neither the proposer nor the miner starts.
    /// The control case proves the decision is not vacuous.
    #[test]
    fn standby_downgrades_an_authority_to_full() {
        assert!(
            matches!(effective_role(Role::Authority, true), Role::Full),
            "standby must run as a full node"
        );
        assert!(
            !miner_should_run(effective_role(Role::Authority, true)),
            "an out-of-date node must not mine"
        );

        assert!(
            matches!(effective_role(Role::Authority, false), Role::Authority),
            "a supported node keeps its authority role"
        );
        assert!(
            miner_should_run(effective_role(Role::Authority, false)),
            "a supported node mines exactly as before"
        );
    }
}

#[cfg(test)]
mod idle_mode_tests {
    use super::*;
    use sc_service::Role;

    /// The deadline boundary is the whole contract: just-before is idle, **at**
    /// the deadline the node is active again, after it stays active.
    #[test]
    fn idle_boundary_is_inclusive_of_the_deadline() {
        assert!(idle_active(Some(1_000), 999), "one second before → idle");
        assert!(!idle_active(Some(1_000), 1_000), "at the deadline → active");
        assert!(!idle_active(Some(1_000), 1_001), "after → active");
        assert!(!idle_active(None, 1_000), "no state file → active");
    }

    /// A restart with an expired file must **mine**, not stay parked: the node
    /// cannot know the process was down, it only sees `until` in the past.
    #[test]
    fn expired_state_file_means_mining_resumes() {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("gpn-node-idle-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("idle.json");

        // Written by the GUI/an operator; deadline already passed.
        std::fs::write(&p, r#"{"until":1000,"reason":"gaming","started":400}"#).unwrap();
        assert!(!idle_active(read_idle_until(&p), 2_000));

        // A live one parks the miner.
        std::fs::write(&p, r#"{"until":99999,"reason":"gaming","started":1000}"#).unwrap();
        assert!(idle_active(read_idle_until(&p), 2_000));

        // Corrupt / half-written / missing must never park the miner.
        std::fs::write(&p, b"{not json").unwrap();
        assert!(!idle_active(read_idle_until(&p), 2_000));
        std::fs::write(&p, b"{}").unwrap();
        assert!(!idle_active(read_idle_until(&p), 2_000));
        assert!(!idle_active(
            read_idle_until(&dir.join("absent.json")),
            2_000
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Idle must respect the existing role gate — it only ever *removes*
    /// permission to mine, never grants it (a full node stays a full node).
    #[test]
    fn the_miner_gate_respects_idle_and_the_role() {
        assert!(mining_allowed(Role::Authority, false));
        assert!(
            !mining_allowed(Role::Authority, true),
            "idle parks an authority"
        );
        assert!(!mining_allowed(Role::Full, false));
        assert!(!mining_allowed(Role::Full, true));
    }

    /// One log line per transition, never one per poll (the loop runs at 500 ms).
    #[test]
    fn parking_logs_only_on_transitions() {
        let mut log = ParkLog::new();
        // Entering idle: one line.
        assert!(log
            .update(Some("idle"))
            .expect("entering idle must log")
            .contains("idle mode"));
        // Still idle a million polls later: silent.
        assert_eq!(log.update(Some("idle")), None);
        // Resume: says so explicitly.
        let line = log.update(None).expect("leaving idle must log");
        assert!(line.contains("idle ended, resuming mining"), "{line}");
        assert_eq!(log.update(None), None);
        // A job taking the GPU is reported as such, not as idle.
        assert!(log
            .update(Some("ai-job"))
            .expect("yield must log")
            .contains("AI job"));
        // Idle ending *into* a job must not claim mining resumed.
        let mut log2 = ParkLog::new();
        log2.update(Some("idle"));
        let line = log2.update(Some("ai-job")).unwrap();
        assert!(line.contains("ai-job"), "{line}");
        assert!(!line.contains("resuming mining"), "{line}");
    }

    /// The one authoring stall that used to leave **no** trace: the miner had no
    /// work for 97 s and the journal said nothing at all. One warning at the
    /// threshold, no nagging while it lasts, one line when it recovers.
    #[test]
    fn a_long_template_gap_is_warned_once_and_its_recovery_is_reported() {
        let mut gap = TemplateGap::new();
        // Gap begins: normal inter-block waiting says nothing.
        assert!(gap.update(false, 0).is_none());
        assert!(gap.update(false, 5_000).is_none());
        // Threshold crossed: exactly one warning, carrying the elapsed time.
        let line = gap
            .update(false, TEMPLATE_GAP_WARN_MS)
            .expect("warn at 30 s");
        assert!(line.warn, "a stall must be a warning");
        assert!(line.text.contains("30.0s"), "{}", line.text);
        assert!(line.text.contains("NO consensus template"), "{}", line.text);
        // Thousands of iterations later: silent, never per-iteration.
        for t in [31_000u128, 60_000, 90_000, 96_000] {
            assert!(gap.update(false, t).is_none(), "must not nag at {t}ms");
        }
        // Recovery is reported with the total gap.
        let line = gap.update(true, 97_500).expect("recovery must log");
        assert!(!line.warn, "recovery is informational");
        assert!(line.text.contains("97.5s"), "{}", line.text);
        // …and then quiet again.
        assert!(gap.update(true, 98_000).is_none());
    }

    /// A short wait between blocks (and a clock that never moves) is silent, and
    /// a backwards jump can never manufacture a stall.
    #[test]
    fn short_or_clamped_template_gaps_stay_silent() {
        let mut gap = TemplateGap::new();
        for t in [0u128, 200, 1_000, 12_000, 29_999] {
            assert!(gap.update(false, t).is_none(), "{t}ms is normal");
        }
        // Template arrives under the threshold: nothing at all is logged.
        assert!(gap.update(true, 29_999).is_none());
        // A clock that appears to run backwards cannot fabricate a gap.
        let mut back = TemplateGap::new();
        assert!(back.update(false, 50_000).is_none());
        assert!(back.update(false, 1_000).is_none());
        assert!(
            back.update(true, 2_000).is_none(),
            "no warning ⇒ no recovery line"
        );
    }
}
