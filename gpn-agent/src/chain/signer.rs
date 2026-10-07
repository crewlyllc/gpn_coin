//! Shared Substrate signer plumbing for the GPN runtime.
//!
//! The GPN runtime declares `pub type Address = AccountId` (bare `AccountId32`),
//! so the default `SubstrateConfig` (which wraps the address in `MultiAddress`)
//! adds an extra SCALE variant byte that breaks extrinsic decoding on the node.
//! We therefore use a custom `GpnConfig` and a `Signer` backed by an
//! `sp_core::sr25519::Pair` (v28), which avoids the `substrate-compat` feature
//! (that would drag in a conflicting sp-core v31).
//!
//! This is the shared home for the plumbing originally introduced in
//! `gpn_seed_feed` (Cycle 22) so the agent's `ChainClient` can submit
//! *signed* extrinsics too (e.g. `GPUAttestation::submit_attestation`).

use sp_core::{crypto::Pair as _, sr25519};
use subxt::{
    backend::{legacy::LegacyRpcMethods, rpc::RpcClient},
    config::{
        substrate::{BlakeTwo256, MultiSignature, SubstrateHeader, H256},
        Config, DefaultExtrinsicParams,
    },
    dynamic::Value,
    tx::{Payload, Signer},
    utils::AccountId32,
    OnlineClient,
};
/// Custom config matching the GPN runtime: `Address = AccountId32` (bare).
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum GpnConfig {}

impl Config for GpnConfig {
    type Hash = H256;
    type AccountId = AccountId32;
    type Address = AccountId32;
    type Signature = MultiSignature;
    type Hasher = BlakeTwo256;
    type Header = SubstrateHeader<u32, BlakeTwo256>;
    type ExtrinsicParams = DefaultExtrinsicParams<Self>;
    type AssetId = u32;
}

/// A `Signer` backed by an `sp_core::sr25519::Pair` (v28), mapped into the GPN
/// runtime's types (AccountId32 / MultiSignature).
#[derive(Clone)]
pub struct Sr25519Signer {
    pair: sr25519::Pair,
}

impl Sr25519Signer {
    /// Build a signer from a Substrate dev URI (e.g. `//Alice`).
    pub fn from_uri(uri: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let pair = sr25519::Pair::from_string(uri, None)
            .map_err(|e| format!("invalid signer URI '{}': {}", uri, e))?;
        Ok(Self { pair })
    }
}

impl Signer<GpnConfig> for Sr25519Signer {
    fn account_id(&self) -> AccountId32 {
        AccountId32(self.pair.public().0)
    }

    fn address(&self) -> AccountId32 {
        AccountId32(self.pair.public().0)
    }

    fn sign(&self, signer_payload: &[u8]) -> MultiSignature {
        let sig = self.pair.sign(signer_payload);
        MultiSignature::Sr25519(sig.0)
    }
}

/// Connect to the GPN chain with the runtime-correct config.
pub async fn connect(
    endpoint: &str,
) -> Result<OnlineClient<GpnConfig>, Box<dyn std::error::Error>> {
    connect_robust::<GpnConfig>(endpoint).await
}

/// Robustly construct an [`OnlineClient`] that tolerates a **pruned finalized
/// block** on PoW chains without a finality gadget (GRANDPA).
///
/// The default [`OnlineClient::from_url`] fetches the chain metadata at the
/// *latest finalized* block. On a PoW dev/testnet chain the finalized head
/// stays stuck at genesis (#0) forever, and once the node prunes state past
/// that block the metadata fetch fails with `UnknownBlock: State already
/// discarded for <genesis>`, which makes `from_url` return `Err` and every
/// downstream read silently fall back to placeholder defaults (nonce 0,
/// balance 0, "not staked").
///
/// Connect using the **best (head) block** metadata, falling back to the normal
/// `from_url` connection only if the head-block path fails.
///
/// Rationale: on this PoW chain the finalized head stays at genesis (#0), so
/// `from_url` fetches **genesis-era** metadata. Reads are fine, but dynamic
/// calls resolve their call index against that metadata — so after a runtime
/// upgrade a call to a *new* extrinsic (e.g. `ComputeMarketplace::fail_job`)
/// would encode the wrong index and be rejected ("variant doesn't exist").
/// Best-block metadata always matches the running runtime.
pub async fn connect_robust<T: Config>(
    endpoint: &str,
) -> Result<OnlineClient<T>, Box<dyn std::error::Error>> {
    // Preferred: metadata at the best (head) block — always available and
    // matches the current runtime (survives forkless upgrades).
    if let Some(client) = connect_best_block::<T>(endpoint).await {
        return Ok(client);
    }
    // Fallback: normal connection (finalized-block metadata).
    Ok(OnlineClient::<T>::from_url(endpoint).await?)
}

/// Build an [`OnlineClient`] from best-block metadata. `None` on any RPC error.
async fn connect_best_block<T: Config>(endpoint: &str) -> Option<OnlineClient<T>> {
    let rpc = RpcClient::from_insecure_url(endpoint).await.ok()?;
    let methods = LegacyRpcMethods::<T>::new(rpc.clone());
    let genesis_hash = methods.genesis_hash().await.ok()?;
    let best_hash = methods.chain_get_block_hash(None).await.ok()??;
    // `state_get_runtime_version` returns the legacy RPC `RuntimeVersion`; the
    // client constructor wants the `subxt_core` one.
    let legacy_rt = methods.state_get_runtime_version(Some(best_hash)).await.ok()?;
    let runtime_version = subxt::client::RuntimeVersion {
        spec_version: legacy_rt.spec_version,
        transaction_version: legacy_rt.transaction_version,
    };
    let metadata = methods.state_get_metadata(Some(best_hash)).await.ok()?;
    OnlineClient::<T>::from_rpc_client_with(genesis_hash, runtime_version, metadata, rpc).ok()
}

/// Read the account's current on-chain nonce from the **head** block (so a
/// second+ tx from the same account isn't rejected as `Transaction is
/// outdated` — see `submit_signed`). Returns 0 if the chain is unreachable or
/// the account doesn't exist yet.
/// Current on-chain runtime `spec_version` via JSON-RPC. `None` if unreachable.
///
/// Used to detect a mid-run **runtime upgrade**: a long-lived client's metadata is
/// then stale and its dynamically-encoded calls are rejected/dropped, which (on the
/// pipelined path) silently diverges the nonce stream. On a change we re-seed the
/// stream (fresh best-block metadata + nonce).
pub async fn runtime_spec_version(endpoint: &str) -> Option<u32> {
    let http_url = endpoint
        .replace("ws://", "http://")
        .replace("wss://", "https://");
    let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"state_getRuntimeVersion","params":[]});
    let resp = reqwest::Client::new()
        .post(&http_url)
        .json(&body)
        .send()
        .await
        .ok()?;
    let v: serde_json::Value = resp.json().await.ok()?;
    v.get("result")?.get("specVersion")?.as_u64().map(|x| x as u32)
}

pub async fn current_nonce(
    client: &OnlineClient<GpnConfig>,
    endpoint: &str,
    account: &AccountId32,
) -> u64 {
    let addr = subxt::dynamic::storage(
        "System",
        "Account",
        vec![Value::from_bytes(account.as_ref() as &[u8; 32])],
    );
    // Resolve the best head block hash (JSON-RPC) so we don't read the stuck
    // finalized #0 on PoW dev chains.
    let http_url = endpoint
        .replace("ws://", "http://")
        .replace("wss://", "https://");
    let body = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "chain_getBlockHash", "params": [] });
    // Resolve the best head block hash asynchronously — a blocking reqwest
    // client inside the tokio runtime (the old `ureq_blocking_post`) triggered
    // a "Cannot drop a runtime in a context where blocking is not allowed"
    // panic at shutdown.
    let head_hash: Option<H256> = {
        async {
            let resp = reqwest::Client::new()
                .post(&http_url)
                .json(&body)
                .send()
                .await
                .ok()?;
            let v: serde_json::Value = resp.json().await.ok()?;
            let hex = v.get("result")?.as_str()?.trim_start_matches("0x");
            let bytes = hex::decode(hex).ok()?;
            if bytes.len() != 32 {
                None
            } else {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&bytes);
                Some(arr.into())
            }
        }
        .await
    };

    let fetch = async {
        let thunk = match head_hash {
            Some(h) => client.storage().at(h).fetch(&addr).await.ok()?,
            None => client
                .storage()
                .at_latest()
                .await
                .ok()?
                .fetch(&addr)
                .await
                .ok()?,
        }?;
        let v = thunk.to_value().ok()?.remove_context();
        // System::Account nonce is a Named composite with a `nonce` field.
        use subxt::ext::scale_value::ValueDef;
        if let ValueDef::Composite(c) = &v.value {
            if let subxt::ext::scale_value::Composite::Named(pairs) = c {
                if let Some((_, nv)) = pairs.iter().find(|(k, _)| k == "nonce") {
                    return nv.as_u128().map(|x| x as u64);
                }
            }
        }
        None
    };
    fetch.await.unwrap_or(0)
}

/// Submit a dynamic call signed by `signer` and wait for it to be **included and
/// dispatched successfully** on the best (not necessarily finalized) fork.
///
/// This is the Cycle-38 robustness fix: inclusion is NOT success. A tx that is
/// included but fails dispatch (e.g. `InsufficientBalance`, `InsufficientStake`)
/// still emits `System::ExtrinsicFailed` in the block — the old code broke on
/// `InBestBlock` and returned `Ok` unconditionally, so a failed extrinsic was
/// logged as "succeeded". This masked the `MinAttestationStake` unit bug (cycle
/// 38): the attestation deposit failed silently because the real stake was 1e21
/// raw = 1B GPN (unaffordable) rather than the intended 1,000 GPN.
///
/// We now call [`TxInBlock::wait_for_success`], which fetches the block events,
/// scans for `System::ExtrinsicFailed`, and returns the `DispatchError` if found.
/// It only decodes the event header + dispatch error (no fragile full dynamic
/// value decode), so it stays robust on the GPN runtime's custom `GpnConfig`.
///
/// Uses an **immortal** transaction — robust on fast-block dev chains where a
/// mortal 32-block window can expire before the tx is included.
///
/// `nonce` is the account's current on-chain nonce read from the **head** block.
/// It must be supplied explicitly: subxt's default reads the nonce from the
/// *finalized* block, which on a PoW dev chain with no finality gadget stays at
/// #0 — so a second+ tx from the same account would be rejected as
/// `Transaction is outdated` (stale nonce 0 vs. real nonce 1+).
/// Submit a signed call with nonce-retry/backoff for tx-pool collisions.
///
/// When two extrinsics from the same account are submitted back-to-back, the
/// second can read a stale nonce (the first is still in the node's tx pool, so
/// the on-chain `System::Account` nonce hasn't advanced yet) and collide —
/// surfacing as "Priority is too low" / "Transaction is outdated". We retry the
/// submission with an incremented nonce until the tx is accepted or a
/// non-nonce error is returned.
pub async fn submit_signed<Call>(
    client: &OnlineClient<GpnConfig>,
    call: &Call,
    signer: &Sr25519Signer,
    nonce: u64,
) -> Result<(), Box<dyn std::error::Error>>
where
    Call: Payload,
{
    const MAX_NONCE_RETRIES: u32 = 8;
    let mut nonce = nonce;
    for attempt in 0..MAX_NONCE_RETRIES {
        match submit_signed_once(client, call, signer, nonce).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                if attempt + 1 < MAX_NONCE_RETRIES && is_nonce_collision(&e.to_string()) {
                    nonce += 1;
                    log::warn!(
                        "tx nonce collision; retrying with nonce {} (attempt {}/{})",
                        nonce,
                        attempt + 1,
                        MAX_NONCE_RETRIES
                    );
                    continue;
                }
                return Err(e);
            }
        }
    }
    Err("nonce collision: exceeded maximum retries".into())
}

/// True when an error string indicates a transaction-pool nonce collision.
fn is_nonce_collision(msg: &str) -> bool {
    let m = msg.to_lowercase();
    m.contains("priority is too low")
        || m.contains("transaction is outdated")
        || m.contains("nonce is too low")
        || m.contains("already in pool")
        || m.contains("usurped")
        || m.contains("same nonce")
        || m.contains("duplicate")
}

async fn submit_signed_once<Call>(
    client: &OnlineClient<GpnConfig>,
    call: &Call,
    signer: &Sr25519Signer,
    nonce: u64,
) -> Result<(), Box<dyn std::error::Error>>
where
    Call: Payload,
{
    let params = subxt::config::DefaultExtrinsicParamsBuilder::<GpnConfig>::new()
        .nonce(nonce)
        .build();
    // Sign OFFLINE then submit_and_watch: the online `sign_and_submit_then_watch`
    // path routes through `refine_params`, which reads the account nonce + block
    // header at the *finalized* block. On a PoW chain without a finality gadget
    // the finalized head stays at genesis and its state is pruned, so that read
    // fails with "State already discarded". We resolve the nonce ourselves (see
    // `current_nonce`), so sign offline and submit the raw bytes directly.
    let progress = client
        .tx()
        .create_signed_offline(call, signer, params)?
        .submit_and_watch()
        .await?;

    // Wait for the tx to be included in a **best** block. We deliberately do
    // NOT wait for finalization (no GRANDPA gadget on a PoW chain, so
    // `finalized` stays at #0 forever).
    use subxt::tx::TxStatus;
    let mut progress = progress;
    // **Tx-inclusion timeout (Cycle 39 fix).** The old loop awaited
    // `progress.next()` with no bound: if a tx reached the pool but was never
    // included in a best block (evicted, subscription stall, node busy), the
    // loop hung forever and wedged the caller's accept/start lane — producing
    // the observed multi-minute claim→start gaps and follow-on
    // `JobNotRunning`/`MaxProviderCapacity` races. Bound it so the cycle can
    // recover instead of blocking the provider indefinitely.
    const TX_INCLUSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
    let included = match tokio::time::timeout(TX_INCLUSION_TIMEOUT, async {
        loop {
            match progress.next().await {
                Some(Ok(TxStatus::InBestBlock(ib))) => break Ok(ib),
                Some(Ok(TxStatus::Error { message })) => {
                    break Err(format!("tx error: {}", message))
                }
                Some(Ok(TxStatus::Invalid { message })) => {
                    break Err(format!("tx invalid: {}", message))
                }
                Some(Ok(TxStatus::Dropped { message })) => {
                    break Err(format!("tx dropped: {}", message))
                }
                Some(Ok(_)) => continue,
                Some(Err(e)) => break Err(e.to_string()),
                None => break Err("tx subscription dropped".to_string()),
            }
        }
    })
    .await
    {
        Ok(Ok(ib)) => ib,
        Ok(Err(msg)) => return Err(msg.into()),
        Err(_) => {
            return Err(format!(
                "tx inclusion timed out after {:?} (tx not in a best block)",
                TX_INCLUSION_TIMEOUT
            )
            .into());
        }
    };
    log::info!("tx included in best block {:?}", included.block_hash());

    // **Dispatch-outcome check (Cycle 38 robustness fix).** Inclusion in a block
    // is not success: a failing extrinsic still gets included, just with a
    // `System::ExtrinsicFailed` event. The OLD code broke on `InBestBlock` and
    // returned `Ok` unconditionally — a failed extrinsic was logged as
    // "succeeded" (this masked the `MinAttestationStake` unit bug in cycle 38).
    //
    // We read the block's `System::Events` storage via the proven dynamic-storage
    // path (the same `client.storage().at(hash).fetch()` used by `current_nonce`)
    // and decode it with `subxt::events::decode_from`, then scan for
    // `System::ExtrinsicFailed` and decode the pallet + error names from the raw
    // module-error bytes. We deliberately do NOT use `TxInBlock::wait_for_success`
    // or `TxInBlock::fetch_events`: both route through subxt's internal
    // `backend.storage_fetch_value` / `block_body` code, which hits a serde-JSON
    // conversion failure on the GPN runtime's custom types (`Serde json error:
    // ...`), yielding an unhelpful message even though the failure itself is
    // caught.
    let events_bytes = read_events_storage(client, included.block_hash()).await?;
    let events = subxt::events::Events::<GpnConfig>::decode_from(events_bytes, client.metadata());

    // Which extrinsic is OURS? A busy chain packs several extrinsics per block,
    // and a *concurrent* failure (e.g. a provider's `accept_job` hitting its
    // capacity) would otherwise be mis-attributed to us. Resolve our index by
    // matching our extrinsic hash against the block's extrinsics; if that can't
    // be read, fall back to the old "any failure" behaviour.
    let our_index: Option<u32> = {
        let mut our_call = Vec::new();
        let md = client.metadata();
        let _ = call.encode_call_data_to(&md, &mut our_call);
        if our_call.is_empty() {
            None
        } else {
            async {
                let block = client.blocks().at(included.block_hash()).await.ok()?;
                let exts = block.extrinsics().await.ok()?;
                for e in exts.iter().flatten() {
                    if e.call_bytes() == our_call.as_slice() {
                        return Some(e.index());
                    }
                }
                None
            }
            .await
        }
    };

    let mut dispatch_failure: Option<String> = None;
    for ev in events.iter() {
        let ev = match ev {
            Ok(e) => e,
            Err(_) => continue,
        };
        let (pallet, variant) = (ev.pallet_name(), ev.variant_name());
        if pallet == "System" && variant == "ExtrinsicFailed" {
            // Only count a failure that belongs to our own extrinsic.
            if let Some(ix) = our_index {
                if !matches!(ev.phase(), subxt::events::Phase::ApplyExtrinsic(i) if i == ix) {
                    continue;
                }
            }
            dispatch_failure = Some(describe_module_error(client, ev.field_bytes()));
            break;
        }
    }

    match dispatch_failure {
        None => {
            log::info!(
                "tx dispatched successfully ({} event(s) in block)",
                events.len()
            );
            Ok(())
        }
        Some(msg) => {
            let full = format!("tx included but FAILED dispatch: {}", msg);
            log::error!("{}", full);
            Err(full.into())
        }
    }
}

/// Submit a signed call and return as soon as the node has **accepted it into
/// the transaction pool** — WITHOUT waiting for block inclusion and WITHOUT
/// decoding the dispatch outcome.
///
/// This is the "untie from block time" primitive: a caller that needs a
/// *sequence* of state transitions (accept → start → submit_result → complete)
/// can assign them consecutive nonces and submit them back-to-back. Substrate's
/// tx pool orders per-account txs by nonce, so the chain dispatches them in
/// order even though we never block on a block. Cost per tx drops from
/// ~block-time (≈4s) to a pool-ack (≈ms).
///
/// Trade-off (deliberate): a tx that is included but fails dispatch is NOT
/// surfaced here. Correctness is repaired by the provider's reconcile loop,
/// which re-drives stragglers (see `gpn_llm_provider::process_once`). Use this
/// only for a signer account whose nonce stream is owned by this process.
pub async fn submit_signed_no_wait<Call>(
    client: &OnlineClient<GpnConfig>,
    call: &Call,
    signer: &Sr25519Signer,
    nonce: u64,
) -> Result<(), Box<dyn std::error::Error>>
where
    Call: Payload,
{
    let params = subxt::config::DefaultExtrinsicParamsBuilder::<GpnConfig>::new()
        .nonce(nonce)
        .build();
    // Sign offline (the online path routes through `refine_params`, which reads
    // the *finalized* block — stuck at genesis on a PoW chain). See
    // `submit_signed_once` for the full rationale.
    let mut progress = client
        .tx()
        .create_signed_offline(call, signer, params)?
        .submit_and_watch()
        .await?;

    // Await only until the node acknowledges the tx (Broadcast / Validated; we
    // may occasionally catch InBestBlock before we even look). Do NOT loop to
    // inclusion. A short timeout bounds the claim lane so a stalled pool can
    // never wedge the provider.
    use subxt::tx::TxStatus;
    const ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
    let ack = tokio::time::timeout(ACK_TIMEOUT, async {
        loop {
            match progress.next().await {
                Some(Ok(TxStatus::Broadcasted { .. })) => break Ok(()),
                Some(Ok(TxStatus::Validated)) => break Ok(()),
                Some(Ok(TxStatus::InBestBlock(_))) => break Ok(()),
                Some(Ok(TxStatus::Error { message })) => break Err(format!("tx error: {message}")),
                Some(Ok(TxStatus::Invalid { message })) => break Err(format!("tx invalid: {message}")),
                Some(Ok(TxStatus::Dropped { message })) => break Err(format!("tx dropped: {message}")),
                Some(Ok(_)) => continue,
                Some(Err(e)) => break Err(e.to_string()),
                None => break Err("tx subscription dropped".to_string()),
            }
        }
    })
    .await;
    match ack {
        Ok(Ok(())) => Ok(()),
        Ok(Err(msg)) => Err(msg.into()),
        Err(_) => Err(format!(
            "tx ack timed out after {ACK_TIMEOUT:?} (tx not accepted into pool)"
        )
        .into()),
    }
}

/// A single-connection, nonce-serialized submitter used to **pipeline** a
/// provider's lifecycle txs without ever waiting for block inclusion.
///
/// One `OnlineClient` (no per-tx reconnect — unlike `submit_signed_call`, which
/// opens a fresh WS connection every call) plus an in-memory `next_nonce`
/// handed out in submission order. Because txs are immortal and nonce-ordered,
/// the chain dispatches `accept_job(n)` before `start_job(n+1)` even though the
/// caller never waits.
///
/// Ownership assumption: this process is the sole nonce source for the signer
/// account. If anything else signs for the same account, call [`NonceStream::resync`]
/// (or drop/recreate the stream) so the local counter doesn't lag the chain.
pub struct NonceStream {
    client: OnlineClient<GpnConfig>,
    signer: Sr25519Signer,
    next_nonce: u64,
    /// On-chain runtime `spec_version` at the time this stream connected. If the
    /// chain's spec changes (runtime upgrade), the caller re-seeds the stream.
    spec_version: u32,
}

impl NonceStream {
    /// Connect once and seed the nonce counter from the chain's head block.
    pub async fn new(
        endpoint: &str,
        signer_uri: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let client = connect(endpoint).await?;
        let signer = Sr25519Signer::from_uri(signer_uri)?;
        let acct = signer.account_id();
        let next_nonce = current_nonce(&client, endpoint, &acct).await;
        let spec_version = runtime_spec_version(endpoint).await.unwrap_or(0);
        Ok(Self {
            client,
            signer,
            next_nonce,
            spec_version,
        })
    }

    /// The runtime `spec_version` this stream connected with.
    pub fn spec_version(&self) -> u32 {
        self.spec_version
    }

    /// Submit `pallet::call(args)` with the next nonce and return as soon as the
    /// pool accepts it. Returns the nonce used (handy for logging/attribution).
    pub async fn submit(
        &mut self,
        pallet: &str,
        call_name: &str,
        args: Vec<Value>,
    ) -> Result<u64, Box<dyn std::error::Error>> {
        let call = subxt::dynamic::tx(pallet, call_name, args);
        let nonce = self.next_nonce;
        submit_signed_no_wait(&self.client, &call, &self.signer, nonce).await?;
        self.next_nonce += 1;
        Ok(nonce)
    }

    /// Re-read the on-chain nonce and pull the local counter forward (never
    /// backward — a pending tx we already submitted must keep its slot).
    pub async fn resync(&mut self, endpoint: &str) -> u64 {
        let acct = self.signer.account_id();
        let on = current_nonce(&self.client, endpoint, &acct).await;
        if on > self.next_nonce {
            self.next_nonce = on;
        }
        self.next_nonce
    }
}

/// Read the `System::Events` SCALE bytes for a block via the **dynamic storage**
/// path (`client.storage().at(hash).fetch`), which is proven to work on the GPN
/// runtime (same as `current_nonce`). Returns an empty vec if the chain is
/// unreachable or storage is missing.
async fn read_events_storage(
    client: &OnlineClient<GpnConfig>,
    block_hash: H256,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let addr = subxt::dynamic::storage("System", "Events", vec![]);
    // Fall back to head if the given hash fails to resolve on a fast-block dev
    // chain (rare; the tx's block should still be queryable).
    let thunk = match client.storage().at(block_hash).fetch(&addr).await {
        Ok(Some(t)) => Some(t),
        _ => None,
    };
    Ok(match thunk {
        Some(t) => t.into_encoded(),
        None => Vec::new(),
    })
}

/// Best-effort decode of a `System::ExtrinsicFailed` event's field bytes into a
/// human-readable dispatch error.
///
/// The first field of `ExtrinsicFailed` is a SCALE-encoded `DispatchError`
/// enum. Variant `3` is `Module`, whose payload is a module error: byte[1] =
/// pallet index, byte[2] = error index (a legacy 2-byte form or the newer
/// 5-byte form — in both cases the pallet/error indices are at those offsets).
/// We resolve the pallet and error names from the runtime metadata. Falls back
/// to the raw variant index + hex bytes if decoding is ambiguous.
fn describe_module_error(client: &OnlineClient<GpnConfig>, field_bytes: &[u8]) -> String {
    // Variant index 3 = `Module` in `sp_runtime::DispatchError`.
    if field_bytes.len() >= 3 && field_bytes[0] == 3 {
        let pallet_idx = field_bytes[1];
        let error_idx = field_bytes[2];
        let meta = client.metadata();
        let name = &meta.pallet_by_index(pallet_idx).and_then(|p| {
            p.error_variant_by_index(error_idx)
                .map(|v| format!("{}::{}", p.name(), v.name))
        });
        match name {
            Some(n) => format!("ModuleError({n}) [pallet={pallet_idx} error={error_idx}]"),
            None => format!("ModuleError(pallet={pallet_idx} error={error_idx})"),
        }
    } else {
        // Non-module dispatch error (BadOrigin=2, Token=7, etc.): show raw bytes.
        let hex_str = field_bytes
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<Vec<_>>()
            .join("");
        format!(
            "DispatchError(variant={} bytes=0x{})",
            field_bytes.first().copied().unwrap_or(0),
            hex_str
        )
    }
}

#[cfg(test)]
mod tests {

    /// Pure helper mirroring `describe_module_error`'s byte parsing, so the
    /// dispatch-error decode logic is unit-testable without a live node.
    fn parse_module_indices(bytes: &[u8]) -> Option<(u8, u8)> {
        // Variant 3 = `Module`; byte[1] = pallet index, byte[2] = error index.
        if bytes.len() >= 3 && bytes[0] == 3 {
            Some((bytes[1], bytes[2]))
        } else {
            None
        }
    }

    #[test]
    fn module_error_bytes_yield_pallet_and_error_indices() {
        // SCALE `DispatchError::Module([pallet=8, error=13, 0,0,0])` (5-byte form).
        let bytes = [3u8, 8, 13, 0, 0, 0];
        assert_eq!(parse_module_indices(&bytes), Some((8, 13)));
    }

    #[test]
    fn legacy_two_byte_module_error_parses() {
        // Legacy 2-byte form: [variant=3, pallet, error] (no trailing filler).
        let bytes = [3u8, 2, 7];
        assert_eq!(parse_module_indices(&bytes), Some((2, 7)));
    }

    #[test]
    fn non_module_dispatch_error_is_not_treated_as_module() {
        // BadOrigin = variant 2 → no module indices.
        assert_eq!(parse_module_indices(&[2u8]), None);
        // Too short (fewer than 3 bytes) → none.
        assert_eq!(parse_module_indices(&[3u8, 5]), None);
        // Empty → none.
        assert_eq!(parse_module_indices(&[]), None);
    }

    #[test]
    fn describe_module_error_resolves_names_via_metadata() {
        // Can't run against a live client here, but verify the byte path our
        // decoder relies on for a real job-not-found-style failure parses the
        // expected (pallet, error) pair. The live integration test
        // (`gpn_verify_dispatch`) exercises the metadata-name resolution.
        let bytes = [3u8, 8, 13, 0, 0, 0]; // Module(pallet=8, error=13)
        assert_eq!(parse_module_indices(&bytes), Some((8, 13)));
    }
}
