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
        substrate::{BlakeTwo256, H256, MultiSignature, SubstrateHeader},
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
pub async fn connect(endpoint: &str) -> Result<OnlineClient<GpnConfig>, Box<dyn std::error::Error>> {
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
        vec![Value::from_bytes((account.as_ref() as &[u8; 32]).to_vec())],
    );
    // Resolve the best head block hash (JSON-RPC) so we don't read the stuck
    // finalized #0 on PoW dev chains.
    let http_url = endpoint.replace("ws://", "http://").replace("wss://", "https://");
    let body = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "chain_getBlockHash", "params": [] });
    // Resolve the best head block hash asynchronously — a blocking reqwest
    // client inside the tokio runtime (the old `ureq_blocking_post`) triggered
    // a "Cannot drop a runtime in a context where blocking is not allowed"
    // panic at shutdown.
    let head_hash: Option<H256> = {
        async {
            let resp = reqwest::Client::new().post(&http_url).json(&body).send().await.ok()?;
            let v: serde_json::Value = resp.json().await.ok()?;
            let hex = v.get("result")?.as_str()?.trim_start_matches("0x");
            let bytes = hex::decode(hex).ok()?;
            if bytes.len() != 32 { None } else {
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
            None => client.storage().at_latest().await.ok()?.fetch(&addr).await.ok()?,
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

/// Submit a dynamic call signed by `signer` and wait for it to be included and
/// succeed on the best (not necessarily finalized) fork. Logs the emitted
/// events; returns `Ok(())` on success.
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
    // `finalized` stays at #0 forever) and we do NOT decode block events here
    // (their dynamic value decoding is fragile). Callers confirm success by
    // reading the relevant storage back (e.g. `get_provider_state`).
    use subxt::tx::TxStatus;
    let mut progress = progress;
    loop {
        match progress.next().await {
            Some(Ok(TxStatus::InBestBlock(_))) => break,
            Some(Ok(TxStatus::Error { message })) => return Err(format!("tx error: {}", message).into()),
            Some(Ok(TxStatus::Invalid { message })) => return Err(format!("tx invalid: {}", message).into()),
            Some(Ok(TxStatus::Dropped { message })) => return Err(format!("tx dropped: {}", message).into()),
            Some(Ok(_)) => continue,
            Some(Err(e)) => return Err(e.into()),
            None => return Err("tx subscription dropped".into()),
        }
    }
    log::info!("tx included in a best block");
    Ok(())
}
