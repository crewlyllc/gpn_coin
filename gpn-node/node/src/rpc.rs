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

use codec::{Decode, Encode};
use jsonrpsee::core::RpcResult;
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::types::ErrorObject;
use sc_client_api::StorageProvider;
use sc_client_api::UsageProvider;
use sc_transaction_pool::TransactionPoolHandle;
use sc_transaction_pool_api::{TransactionPool, TransactionSource};
use serde::{Deserialize, Serialize};
use sp_api::Core;
use sp_api::ProvideRuntimeApi;
use sp_blockchain::HeaderBackend;
use sp_core::storage::StorageKey;
use sp_core::{sr25519, Pair, H256, U256};
use sp_io::hashing::twox_128;
use sp_runtime::generic::{Era, SignedPayload};
use sp_runtime::traits::{BlakeTwo256, Hash};
use sp_runtime::MultiSignature;
use std::sync::{Arc, Mutex};

use crate::challenge::{provider_challenge, verify_challenge_solution};
use crate::service::{FullClient, MiningWorker};
use gpn_runtime::{
    GpnLivenessApi, GpnTreasuryApi, ProfitabilityOracleApi, RuntimeCall, SignedExtra,
    UncheckedExtrinsic,
};
use pallet_gpu_attestation::Call as AttestationCall;

/// The on-chain **bond-weighted median** profitability feed (Phase C
/// aggregation) as returned by the `gpn_effectiveFeed` RPC. Mirrors
/// `pallet_profitability_oracle::ProfitabilityFeed`, serialized for the wire
/// (raw integer fields + a human-readable USD/hr convenience field).
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveFeed {
    /// Token price in micro-USD (1e-6 USD).
    pub token_price_usd: u64,
    /// Average GPU-hour price in micro-USD (1e-6 USD).
    pub avg_gpu_hour_price: u64,
    /// GPU utilization as basis points (1e4 = 100%).
    pub utilization_rate_bps: u32,
    /// Staking APR as basis points (1e4 = 100%).
    pub staking_apr_bps: u32,
    /// Block number when the newest contributing feed was posted.
    pub updated_at: u64,
    /// Human-readable convenience fields.
    pub gpu_hour_price_usd: f64,
    pub utilization: f64,
    pub staking_apr: f64,
    pub token_price: f64,
}

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

    /// Read the **on-chain** Phase-C bond-weighted median profitability feed
    /// (the authoritative `ProfitabilityOracle::effective_feed()`), or `None`
    /// if no bonded operator has posted a feed yet. This is the single source
    /// of truth for the aggregate — a client can call this instead of
    /// reimplementing the bond-weighted median client-side.
    #[method(name = "gpn_effectiveFeed")]
    async fn effective_feed(&self) -> RpcResult<Option<EffectiveFeed>>;
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

    async fn effective_feed(&self) -> RpcResult<Option<EffectiveFeed>> {
        let at = self.client.info().best_hash;
        let api = self.client.runtime_api();
        let feed = api.effective_feed(at).map_err(|e| {
            ErrorObject::owned(-32000, format!("runtime api error: {e}"), None::<()>)
        })?;
        Ok(feed.map(|f| EffectiveFeed {
            token_price_usd: f.token_price_usd,
            avg_gpu_hour_price: f.avg_gpu_hour_price,
            utilization_rate_bps: f.utilization_rate_bps,
            staking_apr_bps: f.staking_apr_bps,
            updated_at: f.updated_at,
            gpu_hour_price_usd: f.avg_gpu_hour_price as f64 / 1e6,
            utilization: f.utilization_rate_bps as f64 / 1e4,
            staking_apr: f.staking_apr_bps as f64 / 1e4,
            token_price: f.token_price_usd as f64 / 1e6,
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
        let ok =
            tokio::task::spawn_blocking(move || futures::executor::block_on(worker.submit(seal)))
                .await
                .map_err(|e| {
                    ErrorObject::owned(-32000, format!("submit task failed: {e}"), None::<()>)
                })?;

        Ok(ok)
    }
}

/// Convenience: build an `RpcModule` with the GPN miner RPC registered.
pub fn create_full_rpc(
    worker: Arc<Mutex<Option<MiningWorker>>>,
    client: Arc<FullClient>,
    pool: Arc<TransactionPoolHandle<gpn_runtime::Block, FullClient>>,
) -> jsonrpsee::RpcModule<()> {
    let mut module = jsonrpsee::RpcModule::new(());
    let rpc = GpnMinerRpc::new(worker, client.clone());
    module
        .merge(rpc.into_rpc())
        .expect("gpn miner rpc methods are unique");
    let pallet_rpc = GpnPalletRpc::new(client.clone());
    module
        .merge(pallet_rpc.into_rpc())
        .expect("gpn pallet rpc methods are unique");
    let challenge_rpc = GpnChallengeRpc::new(client, pool);
    module
        .merge(challenge_rpc.into_rpc())
        .expect("gpn challenge rpc methods are unique");
    module
}

// ###########################################################################
// Capacity-challenge verifier RPC (item 6, Phase-2 native challenge verify)
// ###########################################################################
//
// This is the **node service task** half of `design-verifiable-gpu-proof-2026-10-06.md`
// §"native challenge verify". The runtime cannot run Ethash (no cache, no dep),
// so the *node* (which holds the epoch light cache) verifies a provider's
// capacity-challenge solution natively — exactly like a block seal — and, when
// valid, commits the boolean outcome on-chain via the verifier-only
// `GPUAttestation::confirm_challenge` extrinsic.
//
// Flow (all node-local):
//   1. read the live `GPUAttestation::AttestationChallenge` from chain state;
//   2. `challenge::verify_challenge_solution(challenge, seal, target)` (Ethash);
//   3. on success, sign `confirm_challenge(who)` with the configured verifier key
//      and submit it to the transaction pool.
//
// Config (env, read lazily at call time so the node still starts without them):
//   * `GPN_CHALLENGE_VERIFIER_URI` — sr25519 SURI of the trusted verifier account
//     (must equal the runtime `GPUAttestation::ChallengeVerifier`). Required only
//     to *submit*; verification works without it.
//   * `GPN_CHALLENGE_TARGET`     — hex PoW target (hash < target = valid). Defaults
//     to a dev-easy 2^240 testnet policy. (The runtime stores no per-tier target
//     yet; the verifier owns the policy.)

/// Pallet prefix as named in `construct_runtime!` (`GPUAttestation: pallet_gpu_attestation`).
const ATTESTATION_PREFIX: &[u8] = b"GPUAttestation";
/// Storage item name of the live capacity challenge.
const CHALLENGE_ITEM: &[u8] = b"AttestationChallenge";

/// Result of a `gpn_confirmChallenge` call.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmChallengeResult {
    /// The live on-chain challenge the seal was verified against (hex).
    pub challenge: String,
    /// The **provider-bound** challenge actually mined/verified (hex) —
    /// `blake2_256(tag ++ challenge ++ who)`. A miners fetches this via
    /// `gpn_challengeFor` and mines over it (red-team finding 2026-10-09).
    pub provider_challenge: String,
    /// Whether the seal is a valid Ethash solution to the challenge at `target`.
    pub verified: bool,
    /// Whether a `confirm_challenge` extrinsic was accepted into the pool.
    pub submitted: bool,
    /// The submitted extrinsic hash, when `submitted`.
    pub tx_hash: Option<String>,
    /// Human-readable reason when `verified`/`submitted` is false.
    pub reason: Option<String>,
}

/// RPC surface for the node-native capacity-challenge verifier.
#[rpc(client, server)]
pub trait GpnChallengeApi {
    /// Verify a provider's Ethash solution to the live capacity challenge and,
    /// if valid, commit the result on-chain via the trusted verifier account.
    ///
    /// `provider` is the SS58 address (or 0x-hex 32-byte account) the solution is
    /// attributed to; `seal` is hex `nonce(8 LE) ++ mix_hash(32)` (40 bytes).
    #[method(name = "gpn_confirmChallenge")]
    async fn confirm_challenge(
        &self,
        provider: String,
        seal: String,
    ) -> RpcResult<ConfirmChallengeResult>;

    /// Return the exact **provider-bound** 32-byte challenge a given account must
    /// mine an Ethash solution over: `blake2_256(tag ++ live_challenge ++ who)`,
    /// hex-encoded. This is the single source of truth for miners — the same
    /// derivation `gpn_confirmChallenge` verifies against — so one provider's
    /// solution can never be replayed for another account (red-team finding
    /// 2026-10-09). `provider` is SS58 or 0x-hex 32-byte account.
    #[method(name = "gpn_challengeFor")]
    async fn challenge_for(&self, provider: String) -> RpcResult<String>;
}

/// The twox128-prefixed storage key for `pallet_gpu_attestation::AttestationChallenge`.
fn challenge_storage_key() -> StorageKey {
    let mut key = Vec::with_capacity(32);
    key.extend_from_slice(&twox_128(ATTESTATION_PREFIX));
    key.extend_from_slice(&twox_128(CHALLENGE_ITEM));
    StorageKey(key)
}

/// Parse a big-endian hex PoW target (≤32 bytes, optional `0x`). Returns `None`
/// on invalid hex or an empty/oversized value.
fn parse_target_hex(h: &str) -> Option<U256> {
    let s = h.trim().trim_start_matches("0x");
    let bytes = hex::decode(s).ok()?;
    if bytes.is_empty() || bytes.len() > 32 {
        return None;
    }
    let mut buf = [0u8; 32];
    buf[32 - bytes.len()..].copy_from_slice(&bytes);
    Some(U256::from_big_endian(&buf))
}

/// The verifier's PoW target (hash < target = valid). `GPN_CHALLENGE_TARGET` as
/// big-endian hex; defaults to 2^240 (dev-easy testnet policy).
fn resolve_challenge_target() -> U256 {
    if let Ok(h) = std::env::var("GPN_CHALLENGE_TARGET") {
        if let Some(t) = parse_target_hex(&h) {
            return t;
        }
        log::warn!("GPN_CHALLENGE_TARGET ignored (bad hex); using default 2^240");
    }
    // 2^240: big-endian byte[1] = 0x01.
    let mut buf = [0u8; 32];
    buf[1] = 0x01;
    U256::from_big_endian(&buf)
}

/// Load the trusted verifier key from `GPN_CHALLENGE_VERIFIER_URI`.
fn verifier_pair() -> Result<sr25519::Pair, String> {
    let uri = std::env::var("GPN_CHALLENGE_VERIFIER_URI")
        .map_err(|_| "GPN_CHALLENGE_VERIFIER_URI not set".to_string())?;
    sr25519::Pair::from_string(&uri, None).map_err(|e| format!("bad verifier URI: {e:?}"))
}

/// Build the signed `confirm_challenge` extrinsic the verifier submits.
///
/// The runtime version / genesis hash are taken as explicit arguments (the caller
/// reads them from the client) so this stays pure and unit-testable — deriving
/// them from `sp_io` would require Externalities, which the node host lacks.
/// `nonce` is the verifier account's current `System::Account` nonce.
pub(crate) fn sign_confirm_challenge_extrinsic(
    who: gpn_runtime::AccountId,
    signer: &sr25519::Pair,
    nonce: u32,
    spec_version: u32,
    tx_version: u32,
    genesis_hash: H256,
) -> Result<UncheckedExtrinsic, String> {
    use frame_system::{
        CheckEra, CheckGenesis, CheckNonce, CheckSpecVersion, CheckTxVersion, CheckWeight,
    };
    use gpn_runtime::Runtime;
    use pallet_transaction_payment::ChargeTransactionPayment;

    let call = RuntimeCall::GPUAttestation(AttestationCall::confirm_challenge { who });
    let extra: SignedExtra = (
        CheckSpecVersion::<Runtime>::new(),
        CheckTxVersion::<Runtime>::new(),
        CheckGenesis::<Runtime>::new(),
        CheckEra::<Runtime>::from(Era::immortal()),
        CheckNonce::<Runtime>::from(nonce),
        CheckWeight::<Runtime>::new(),
        ChargeTransactionPayment::<Runtime>::from(0u128),
    );

    // Immortal era: genesis is both the genesis-check and era-check implicit.
    let implicit = (
        spec_version,
        tx_version,
        genesis_hash,
        genesis_hash,
        (),
        (),
        (),
    );
    let payload =
        SignedPayload::<RuntimeCall, SignedExtra>::from_raw(call.clone(), extra.clone(), implicit);
    let signature = payload.using_encoded(|bytes| signer.sign(bytes));

    let signer_account = gpn_runtime::AccountId::from(signer.public().0);
    Ok(UncheckedExtrinsic::new_signed(
        call,
        signer_account,
        MultiSignature::Sr25519(signature),
        extra,
    ))
}

/// Implementation backed by the client's storage view + the transaction pool.
pub struct GpnChallengeRpc {
    client: Arc<FullClient>,
    pool: Arc<TransactionPoolHandle<gpn_runtime::Block, FullClient>>,
}

impl GpnChallengeRpc {
    pub fn new(
        client: Arc<FullClient>,
        pool: Arc<TransactionPoolHandle<gpn_runtime::Block, FullClient>>,
    ) -> Self {
        Self { client, pool }
    }

    /// Read the live global `AttestationChallenge` from chain state.
    fn read_live_challenge(&self, at: sp_core::H256) -> RpcResult<[u8; 32]> {
        let data = self
            .client
            .storage(at, &challenge_storage_key())
            .map_err(|e| ErrorObject::owned(-32000, format!("storage error: {e}"), None::<()>))?
            .ok_or_else(|| {
                ErrorObject::owned(-32000, "no AttestationChallenge on chain", None::<()>)
            })?;
        <[u8; 32]>::decode(&mut &data.0[..])
            .map_err(|e| ErrorObject::owned(-32000, format!("challenge decode: {e}"), None::<()>))
    }
}

#[jsonrpsee::core::async_trait]
impl GpnChallengeApiServer for GpnChallengeRpc {
    async fn confirm_challenge(
        &self,
        provider: String,
        seal: String,
    ) -> RpcResult<ConfirmChallengeResult> {
        use frame_system_rpc_runtime_api::AccountNonceApi;

        let who = parse_account(&provider)?;
        let at = self.client.info().best_hash;

        // 1. Read the live challenge from chain state.
        let challenge = self.read_live_challenge(at)?;
        let challenge_hex = format!("0x{}", hex::encode(challenge));

        // 2. Parse + verify the seal natively, against the **provider-bound**
        //    challenge (red-team finding 2026-10-09). The on-chain challenge is a
        //    single global value, so verifying the raw value here would let one
        //    Ethash solution node-verify unlimited accounts (each call naming a
        //    different `who`) — a sybil bypass of the Silver/Gold challenge gate.
        //    Binding the derivation to `who` means a solution is valid for exactly
        //    the account it was mined for.
        let bound = provider_challenge(&challenge, &who.encode());
        let bound_hex = format!("0x{}", hex::encode(bound));
        let seal_bytes = GpnMinerRpc::decode_hex(&seal)?;
        let target = resolve_challenge_target();
        let verified = verify_challenge_solution(&bound, &seal_bytes, target);
        if !verified {
            return Ok(ConfirmChallengeResult {
                challenge: challenge_hex,
                provider_challenge: bound_hex,
                verified: false,
                submitted: false,
                tx_hash: None,
                reason: Some(
                    "seal is not a valid Ethash solution to this provider's bound challenge"
                        .to_string(),
                ),
            });
        }

        // 3. Commit the result on-chain, signed by the trusted verifier.
        let signer = verifier_pair().map_err(|e| {
            ErrorObject::owned(
                -32000,
                format!("verified, but cannot submit: {e}"),
                None::<()>,
            )
        })?;
        let verifier_account = gpn_runtime::AccountId::from(signer.public().0);
        let nonce = self
            .client
            .runtime_api()
            .account_nonce(at, verifier_account.clone())
            .map_err(|e| {
                ErrorObject::owned(-32000, format!("nonce runtime api: {e}"), None::<()>)
            })?;
        let version = self.client.runtime_api().version(at).map_err(|e| {
            ErrorObject::owned(-32000, format!("version runtime api: {e}"), None::<()>)
        })?;
        let genesis = self.client.info().genesis_hash;
        let xt = sign_confirm_challenge_extrinsic(
            who.clone(),
            &signer,
            nonce,
            version.spec_version,
            version.transaction_version,
            genesis,
        )
        .map_err(|e| ErrorObject::owned(-32000, format!("extrinsic build: {e}"), None::<()>))?;
        let xt_hash = BlakeTwo256::hash_of(&xt);
        self.pool
            .submit_one(at, TransactionSource::External, xt)
            .await
            .map_err(|e| ErrorObject::owned(-32000, format!("pool submit: {e}"), None::<()>))?;

        log::info!("challenge confirmed for {:?} (tx {:#x})", who, xt_hash);
        Ok(ConfirmChallengeResult {
            challenge: challenge_hex,
            provider_challenge: bound_hex,
            verified: true,
            submitted: true,
            tx_hash: Some(format!("{:#x}", xt_hash)),
            reason: None,
        })
    }

    async fn challenge_for(&self, provider: String) -> RpcResult<String> {
        let who = parse_account(&provider)?;
        let at = self.client.info().best_hash;
        let challenge = self.read_live_challenge(at)?;
        let bound = provider_challenge(&challenge, &who.encode());
        Ok(format!("0x{}", hex::encode(bound)))
    }
}

// ###########################################################################
// Typed pallet RPC (treasury + liveness)
// ###########################################################################

/// Wire form of [`gpn_runtime::TreasuryStatus`]. Balances are string-encoded
/// decimal integers (1 GPN = 1e12 base units; the 1B cap = 1e21 exceeds u64,
/// so we never emit balances as JSON numbers) plus a human-readable `*_gpn`
/// float convenience field.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TreasuryStatus {
    /// SS58-encoded treasury account (`gpn/trsy` PalletId account).
    pub account: String,
    /// Free balance held by the treasury account (decimal string base units).
    pub balance: String,
    /// Total scheduled allocation (decimal string base units).
    pub total_allocation: String,
    /// Amount unlocked by the vesting schedule (decimal string base units).
    pub unlocked: String,
    /// Unlocked minus released — spending headroom (decimal string).
    pub spendable: String,
    /// Total already released/spent (decimal string).
    pub released: String,
    /// Approval threshold (13 for GPN).
    pub threshold: u32,
    /// Governance roster size (25 for GPN).
    pub roster_size: u32,
    /// Next spend-proposal id.
    pub next_proposal_id: u64,
    /// Human-readable convenience fields, in whole GPN.
    pub balance_gpn: f64,
    pub total_allocation_gpn: f64,
    pub unlocked_gpn: f64,
    pub spendable_gpn: f64,
    pub released_gpn: f64,
}

/// Wire form of [`gpn_runtime::LivenessStatus`].
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LivenessStatus {
    /// SS58-encoded queried account.
    pub account: String,
    /// Whether the account is registered as a full node.
    pub registered: bool,
    /// Whether the node is active (false once slashed/unregistered).
    pub active: bool,
    /// Rolling uptime over the lookback window, in basis points.
    pub uptime_bps: u32,
    /// Rolling uptime as a fraction (1.0 = 100%).
    pub uptime: f64,
    /// Window id of the last confirmed-online window.
    pub last_confirmed_window: u64,
    /// Consecutive missed liveness windows.
    pub missed_count: u32,
    /// Bond currently held (decimal string base units).
    pub bond: String,
    /// Human-readable bond in whole GPN.
    pub bond_gpn: f64,
}

/// Typed RPC surface for the GPN pallets (treasury + liveness). Clients can
/// call these instead of issuing fragile dynamic storage reads.
#[rpc(client, server)]
pub trait GpnPalletApi {
    /// The treasury's typed status: account, balances, schedule totals and the
    /// 13-of-25 governance configuration.
    #[method(name = "gpn_treasuryStatus")]
    async fn treasury_status(&self) -> RpcResult<TreasuryStatus>;

    /// Liveness status for one account (SS58 address or `0x`-prefixed 32-byte
    /// hex). Always returns a value; `registered = false` for unknown accounts.
    #[method(name = "gpn_livenessStatus")]
    async fn liveness_status(&self, account: String) -> RpcResult<LivenessStatus>;

    /// Running total of registered full nodes.
    #[method(name = "gpn_fullNodeCount")]
    async fn full_node_count(&self) -> RpcResult<u32>;
}

/// Base-unit divisor: 1 GPN = 1e12 base units.
const BASE_UNITS_PER_GPN: f64 = 1e12;

fn gpn_f64(base_units: u128) -> f64 {
    base_units as f64 / BASE_UNITS_PER_GPN
}

/// Implementation backed by the client's runtime-API view.
pub struct GpnPalletRpc {
    client: Arc<FullClient>,
}

impl GpnPalletRpc {
    pub fn new(client: Arc<FullClient>) -> Self {
        Self { client }
    }
}

/// Parse an SS58 address, or a `0x`-prefixed 32-byte hex account id.
fn parse_account(s: &str) -> RpcResult<gpn_runtime::AccountId> {
    use sp_core::crypto::Ss58Codec;
    if let Some(hex_part) = s.strip_prefix("0x") {
        let bytes = hex::decode(hex_part).map_err(|e| {
            ErrorObject::owned(-32602, format!("invalid hex account: {e}"), None::<()>)
        })?;
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| ErrorObject::owned(-32602, "account hex must be 32 bytes", None::<()>))?;
        Ok(gpn_runtime::AccountId::from(arr))
    } else {
        gpn_runtime::AccountId::from_ss58check(s).map_err(|e| {
            ErrorObject::owned(-32602, format!("invalid SS58 account: {e:?}"), None::<()>)
        })
    }
}

#[jsonrpsee::core::async_trait]
impl GpnPalletApiServer for GpnPalletRpc {
    async fn treasury_status(&self) -> RpcResult<TreasuryStatus> {
        let at = self.client.info().best_hash;
        let api = self.client.runtime_api();
        let s = api.treasury_status(at).map_err(|e| {
            ErrorObject::owned(-32000, format!("runtime api error: {e}"), None::<()>)
        })?;
        Ok(TreasuryStatus {
            account: s.account.to_string(),
            balance: s.balance.to_string(),
            total_allocation: s.total_allocation.to_string(),
            unlocked: s.unlocked.to_string(),
            spendable: s.spendable.to_string(),
            released: s.released.to_string(),
            threshold: s.threshold,
            roster_size: s.roster_size,
            next_proposal_id: s.next_proposal_id,
            balance_gpn: gpn_f64(s.balance),
            total_allocation_gpn: gpn_f64(s.total_allocation),
            unlocked_gpn: gpn_f64(s.unlocked),
            spendable_gpn: gpn_f64(s.spendable),
            released_gpn: gpn_f64(s.released),
        })
    }

    async fn liveness_status(&self, account: String) -> RpcResult<LivenessStatus> {
        let who = parse_account(&account)?;
        let at = self.client.info().best_hash;
        let api = self.client.runtime_api();
        let s = api.liveness_status(at, who).map_err(|e| {
            ErrorObject::owned(-32000, format!("runtime api error: {e}"), None::<()>)
        })?;
        Ok(LivenessStatus {
            account: s.account.to_string(),
            registered: s.registered,
            active: s.active,
            uptime_bps: s.uptime_bps,
            uptime: s.uptime_bps as f64 / 1e4,
            last_confirmed_window: s.last_confirmed_window,
            missed_count: s.missed_count,
            bond: s.bond.to_string(),
            bond_gpn: gpn_f64(s.bond),
        })
    }

    async fn full_node_count(&self) -> RpcResult<u32> {
        let at = self.client.info().best_hash;
        let api = self.client.runtime_api();
        api.full_node_count(at)
            .map_err(|e| ErrorObject::owned(-32000, format!("runtime api error: {e}"), None::<()>))
    }
}

#[cfg(test)]
mod challenge_rpc_tests {
    use super::*;

    #[test]
    fn target_parsing_handles_prefix_and_short_values() {
        // Plain hex is a big-endian integer: a short value is low-order.
        assert_eq!(parse_target_hex("01"), Some(U256::from(1u64)));
        let mut b = [0u8; 32];
        b[30] = 0x01;
        assert_eq!(parse_target_hex("0x0100"), Some(U256::from_big_endian(&b)));
        // 0x-prefixed full-width value round-trips.
        let full = format!("0x{}", "ff".repeat(32));
        assert_eq!(parse_target_hex(&full), Some(U256::MAX));
        // Bad input is rejected (empty / oversized / non-hex).
        assert!(parse_target_hex("").is_none());
        assert!(parse_target_hex("zz").is_none());
        assert!(parse_target_hex(&"ab".repeat(33)).is_none());
    }

    #[test]
    fn confirm_challenge_extrinsic_encodes_and_roundtrips() {
        // A well-known dev key: `//Alice` in sr25519.
        let pair = sr25519::Pair::from_string("//Alice", None).unwrap();
        let who = gpn_runtime::AccountId::from([7u8; 32]);
        let xt = sign_confirm_challenge_extrinsic(who, &pair, 0, 12, 3, H256::repeat_byte(0xAB))
            .unwrap();

        // Must be a signed transaction that re-decodes to the same bytes.
        assert!(xt.is_signed());
        let encoded = xt.encode();
        let decoded = UncheckedExtrinsic::decode(&mut &encoded[..]).unwrap();
        assert_eq!(decoded.encode(), encoded);
    }
}
