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

/// Submit a dynamic call signed by `signer` and wait for it to be included and
/// succeed on the best (not necessarily finalized) fork. Logs the emitted
/// events; returns `Ok(())` on success.
///
/// Uses an **immortal** transaction — robust on fast-block dev chains where a
/// mortal 32-block window can expire before the tx is included.
pub async fn submit_signed<Call>(
    client: &OnlineClient<GpnConfig>,
    call: &Call,
    signer: &Sr25519Signer,
) -> Result<(), Box<dyn std::error::Error>>
where
    Call: Payload,
{
    let params = subxt::config::DefaultExtrinsicParamsBuilder::<GpnConfig>::new().build();
    let progress = client
        .tx()
        .sign_and_submit_then_watch(call, signer, params)
        .await?;
    let events = progress.wait_for_finalized_success().await?;
    for event in events.iter() {
        let ev = event?;
        let field_count = ev.field_values().map(|c| c.len()).unwrap_or(0);
        log::info!(
            "  event: {}::{}({} fields)",
            ev.pallet_name(),
            ev.variant_name(),
            field_count
        );
    }
    Ok(())
}
