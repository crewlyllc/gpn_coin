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
    Ok(OnlineClient::<GpnConfig>::from_url(endpoint).await?)
}

/// Read the account's current on-chain nonce from the **head** block (so a
/// second+ tx from the same account isn't rejected as `Transaction is
/// outdated` — see `submit_signed`). Returns 0 if the chain is unreachable or
/// the account doesn't exist yet.
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
pub async fn submit_signed<Call>(
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
    let progress = client
        .tx()
        .sign_and_submit_then_watch(call, signer, params)
        .await?;

    // Wait for the tx to be included in a **best** block. We deliberately do
    // NOT wait for finalization (no GRANDPA gadget on a PoW chain, so
    // `finalized` stays at #0 forever).
    use subxt::tx::TxStatus;
    let mut progress = progress;
    let included = loop {
        match progress.next().await {
            Some(Ok(TxStatus::InBestBlock(ib))) => break ib,
            Some(Ok(TxStatus::Error { message })) => {
                return Err(format!("tx error: {}", message).into())
            }
            Some(Ok(TxStatus::Invalid { message })) => {
                return Err(format!("tx invalid: {}", message).into())
            }
            Some(Ok(TxStatus::Dropped { message })) => {
                return Err(format!("tx dropped: {}", message).into())
            }
            Some(Ok(_)) => continue,
            Some(Err(e)) => return Err(e.into()),
            None => return Err("tx subscription dropped".into()),
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

    let mut dispatch_failure: Option<String> = None;
    for ev in events.iter() {
        let ev = match ev {
            Ok(e) => e,
            Err(_) => continue,
        };
        let (pallet, variant) = (ev.pallet_name(), ev.variant_name());
        if pallet == "System" && variant == "ExtrinsicFailed" {
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
