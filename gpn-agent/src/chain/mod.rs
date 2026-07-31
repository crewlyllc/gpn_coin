/// Chain client — Substrate RPC interface for the GPN node agent.
///
/// Communicates with the GPN blockchain over subxt (WebSocket JSON-RPC).
///
/// # Live-vs-fallback behaviour
/// The on-chain profitability feed and difficulty are read with subxt **dynamic
/// storage queries** (no codegen — works against any runtime metadata). If the
/// chain is unreachable, or a storage entry is missing/empty, the client
/// degrades gracefully to the previous placeholder defaults and logs a warning,
/// so the agent keeps running (e.g. the `scripted` demo, offline tests).
///
/// ## On-chain data read
/// - `GPUPoW::Difficulty` — little-endian `[u64; 4]` **target** (hash < target
///   = valid, so HIGHER = EASIER). Converted to a classic *difficulty metric*
///   `2^256 / target` for the profitability calculator.
/// - `ProfitabilityOracle::CurrentFeed` — `ProfitabilityFeed` struct with
///   token price / GPU-hour price / utilization / staking APR.
///
/// ## Units
/// - `token_price_usd`, `avg_gpu_hour_price` are stored as **micro-USD**
///   (`1_000_000 == $1.00`).
/// - `utilization_rate_bps`, `staking_apr_bps` are **basis points**
///   (`10_000 == 100%`, `1_200 == 12%`).
/// (Units are a design decision of the oracle pallet; the agent converts to the
/// dollar / fraction / percent forms the profitability calculator expects.)

pub mod signer;

use serde::{Deserialize, Serialize};
use subxt::{
    config::SubstrateConfig,
    dynamic::Value,
    tx::Signer as _,
    OnlineClient,
};

/// Fallback defaults when the chain is unreachable / feed empty. Matches the
/// previous hardcoded placeholders so behaviour is unchanged offline.
const FALLBACK_TOKEN_PRICE_USD: f64 = 0.05;
const FALLBACK_GPU_HOUR_PRICE_USD: f64 = 2.5;
const FALLBACK_UTILIZATION: f64 = 0.65;
const FALLBACK_STAKING_APR_PERCENT: f64 = 12.0;
const FALLBACK_DIFFICULTY: f64 = 100_000_000.0;

/// Parsed on-chain profitability feed (dollars / fraction / percent forms).
#[derive(Clone, Debug)]
pub struct OnChainFeed {
    pub token_price_usd: f64,
    pub avg_gpu_hour_price_usd: f64,
    pub utilization: f64,          // 0.0 ..= 1.0
    pub staking_apr_percent: f64,  // e.g. 12.0 == 12%
    pub updated_at_block: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountInfo {
    pub address: String,
    pub balance: f64,
    pub nonce: u64,
}

#[derive(Clone)]
pub struct ChainClient {
    endpoint: String,
    /// Live subxt client; `None` if we failed to connect (or never tried).
    client: Option<OnlineClient<SubstrateConfig>>,
    /// Signer URI (e.g. `//Alice`) used for submitting signed extrinsics.
    /// `None` = read-only (no signed writes possible).
    signer_uri: Option<String>,
}

impl ChainClient {
    /// Connect to the GPN chain. On failure we log a warning and keep
    /// `client = None` so all reads fall back to placeholder defaults — the
    /// agent must keep working even when the chain is down.
    pub async fn new(endpoint: &str) -> Result<Self, Box<dyn std::error::Error>> {
        log::info!("Connecting to GPN chain at {}", endpoint);
        let client = match OnlineClient::<SubstrateConfig>::from_url(endpoint).await {
            Ok(c) => {
                log::info!("Connected to GPN chain (subxt) at {}", endpoint);
                Some(c)
            }
            Err(e) => {
                log::warn!(
                    "Could not connect to GPN chain at {} ({}). \
                     Falling back to placeholder chain data.",
                    endpoint, e
                );
                None
            }
        };
        Ok(Self {
            endpoint: endpoint.to_string(),
            client,
            signer_uri: None,
        })
    }

    /// Enable signed extrinsic submission with the given signer URI
    /// (e.g. `//Alice` in dev/local chains, or a seed phrase on mainnet).
    pub fn with_signer(mut self, uri: &str) -> Self {
        self.signer_uri = Some(uri.to_string());
        self
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Resolve the current **best (head)** block hash so storage reads see
    /// unfinalized blocks. `at_latest()` reads the latest *finalized* block,
    /// which on chains without a finality gadget (dev/single-node PoW) can be
    /// stuck at genesis while the feed/difficulty live far ahead on the best
    /// fork. Falls back to `None` (caller falls back to placeholders) on error.
    async fn head_block_hash(&self) -> Option<subxt::utils::H256> {
        // Convert ws:// -> http:// for a plain JSON-RPC POST.
        let http_url = self
            .endpoint
            .replace("ws://", "http://")
            .replace("wss://", "https://");
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "chain_getBlockHash",
            "params": []
        });
        let resp: serde_json::Value = reqwest::Client::new()
            .post(&http_url)
            .json(&body)
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;
        let hash_hex = resp.get("result")?.as_str()?;
        let bytes = hex::decode(hash_hex.trim_start_matches("0x")).ok()?;
        let mut arr = [0u8; 32];
        if bytes.len() != 32 {
            return None;
        }
        arr.copy_from_slice(&bytes);
        Some(arr.into())
    }

    /// Read a storage value at the current best head block (or fall back to
    /// finalized via `at_latest` if the head hash can't be resolved).
    async fn storage_at_head<Addr>(&self, client: &OnlineClient<SubstrateConfig>, addr: &Addr) -> Option<subxt::dynamic::Value>
    where
        Addr: subxt::storage::Address<
            Target = subxt::dynamic::DecodedValueThunk,
            IsFetchable = subxt::utils::Yes,
        >,
    {
        let head = self.head_block_hash().await;
        match head {
            Some(hash) => client
                .storage()
                .at(hash)
                .fetch(addr)
                .await
                .ok()?
                .map(|t| t.to_value().ok().map(|v| v.remove_context()))
                .flatten(),
            None => client
                .storage()
                .at_latest()
                .await
                .ok()?
                .fetch(addr)
                .await
                .ok()?
                .map(|t| t.to_value().ok().map(|v| v.remove_context()))
                .flatten(),
        }
    }

    /// Read the on-chain profitability feed. Returns `None` if the chain is
    /// unreachable or the feed has not been set by an oracle operator yet.
    async fn fetch_feed(&self) -> Option<OnChainFeed> {
        let client = self.client.as_ref()?;
        let addr = subxt::dynamic::storage(
            "ProfitabilityOracle",
            "CurrentFeed",
            vec![],
        );
        let fields = self.storage_at_head(client, &addr).await?;
        let token_micro = get_u64_field(&fields, "token_price_usd")?;
        let gpu_micro = get_u64_field(&fields, "avg_gpu_hour_price")?;
        let utilization_bps = get_u32_field(&fields, "utilization_rate_bps")? as u64;
        let apr_bps = get_u32_field(&fields, "staking_apr_bps")? as u64;
        let updated_at = get_u64_field(&fields, "updated_at")?;

        Some(OnChainFeed {
            token_price_usd: token_micro as f64 / 1_000_000.0,
            avg_gpu_hour_price_usd: gpu_micro as f64 / 1_000_000.0,
            utilization: utilization_bps as f64 / 10_000.0,
            staking_apr_percent: apr_bps as f64 / 100.0,
            updated_at_block: updated_at,
        })
    }

    /// Read the on-chain PoW **target** (`[u64;4]` little-endian limbs) and
    /// convert to a classic difficulty metric `2^256 / target`. Falls back to
    /// the placeholder difficulty on any error.
    async fn fetch_difficulty(&self) -> Option<f64> {
        let client = self.client.as_ref()?;
        let addr = subxt::dynamic::storage("GPUPoW", "Difficulty", vec![]);
        let limbs = self.storage_at_head(client, &addr).await?;
        // Pallet stores `[u64; 4]` little-endian limbs.
        let limbs_arr = composite_limbs(&limbs);
        Some(target_to_difficulty(limbs_arr))
    }

    pub async fn get_account_info(&self, address: &str) -> Result<AccountInfo, Box<dyn std::error::Error>> {
        // Best-effort: read System::Account balance if connected, else 0.
        let mut balance = 0.0f64;
        if let Some(client) = &self.client {
            if let Ok(addr32) = address.parse::<subxt::utils::AccountId32>() {
                let storage_addr = subxt::dynamic::storage(
                    "System",
                    "Account",
                    vec![Value::from_bytes((addr32.as_ref() as &[u8; 32]).to_vec())],
                );
                if let Some(val) = self.storage_at_head(client, &storage_addr).await {
                    if let Some(data) = get_field(&val, "data") {
                        if let Some(free) = get_u128_field(data, "free") {
                            balance = free as f64 / 1_000_000_000_000.0;
                        }
                    }
                }
            }
        }
        Ok(AccountInfo {
            address: address.to_string(),
            balance,
            nonce: 0,
        })
    }

    pub async fn get_difficulty(&self) -> Result<f64, Box<dyn std::error::Error>> {
        Ok(self.fetch_difficulty().await.unwrap_or_else(|| {
            log::debug!("Difficulty fallback -> {}", FALLBACK_DIFFICULTY);
            FALLBACK_DIFFICULTY
        }))
    }

    pub async fn get_token_price_usd(&self) -> Result<f64, Box<dyn std::error::Error>> {
        Ok(self.feed_value(|f| f.token_price_usd, FALLBACK_TOKEN_PRICE_USD).await)
    }

    pub async fn get_gpu_hour_price(&self, _model: &str) -> Result<f64, Box<dyn std::error::Error>> {
        Ok(self.feed_value(|f| f.avg_gpu_hour_price_usd, FALLBACK_GPU_HOUR_PRICE_USD).await)
    }

    pub async fn get_utilization_rate(&self, _model: &str) -> Result<f64, Box<dyn std::error::Error>> {
        Ok(self.feed_value(|f| f.utilization, FALLBACK_UTILIZATION).await)
    }

    pub async fn get_staking_apr(&self) -> Result<f64, Box<dyn std::error::Error>> {
        Ok(self.feed_value(|f| f.staking_apr_percent, FALLBACK_STAKING_APR_PERCENT).await)
    }

    /// Fetch a single field from the on-chain feed, falling back to `fallback`
    /// (with a debug log) when the feed is unavailable.
    async fn feed_value<F: Fn(&OnChainFeed) -> f64>(&self, extract: F, fallback: f64) -> f64 {
        match self.fetch_feed().await {
            Some(feed) => extract(&feed),
            None => {
                log::debug!("Profitability feed unavailable -> fallback {}", fallback);
                fallback
            }
        }
    }

    pub async fn get_attestation_challenge(&self) -> Result<[u8; 32], Box<dyn std::error::Error>> {
        // Read `GPUAttestation::AttestationChallenge` from the chain via a
        // dynamic storage query. Falls back to a placeholder only if the
        // chain is unreachable.
        let Some(client) = &self.client else {
            return Ok([0u8; 32]);
        };
        let addr = subxt::dynamic::storage("GPUAttestation", "AttestationChallenge", vec![]);
        match self.storage_at_head(client, &addr).await {
            Some(v) => Ok(composite_to_bytes(&v, 32)),
            None => Ok([0u8; 32]),
        }
    }

    pub async fn submit_attestation(
        &self,
        result: [u8; 32],
        gpu_info_hash: [u8; 32],
        gpu_count: u8,
        compute_gflops: u64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Build the signed `GPUAttestation::submit_attestation` extrinsic using
        // the same custom GpnConfig + Sr25519Signer plumbing as gpn_seed_feed.
        let Some(uri) = &self.signer_uri else {
            log::warn!("No signer configured; attestation NOT submitted (set signer_uri).");
            return Ok(());
        };

        let signer = signer::Sr25519Signer::from_uri(uri)?;
        let client = signer::connect(&self.endpoint).await?;

        log::info!(
            "Submitting GPUAttestation::submit_attestation as {} (gpus={}, {} GFLOPS, challenge={}..)",
            signer.account_id(), gpu_count, compute_gflops, hex::encode(&result[..4])
        );

        let call = subxt::dynamic::tx(
            "GPUAttestation",
            "submit_attestation",
            vec![
                Value::from_bytes(result),
                Value::from_bytes(gpu_info_hash),
                Value::u128(gpu_count as u128),
                Value::u128(compute_gflops as u128),
            ],
        );

        signer::submit_signed(&client, &call, &signer).await?;
        log::info!("GPU attestation submitted on-chain and succeeded.");
        Ok(())
    }

    pub async fn submit_mining_solution(
        &self,
        _nonce: u64,
        _mix_hash: [u8; 32],
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Mining submission goes through the external-mining HTTP RPC
        // (gpn_submitWork) in the mining engine, not here.
        Ok(())
    }
}

/// Convert a little-endian `[u64;4]` target into a classic difficulty metric
/// `2^256 / target`. Target is never zero in practice (pallet clamps it), but
/// we guard anyway to avoid dividing by zero.
fn target_to_difficulty(limbs: [u64; 4]) -> f64 {
    // target = l0 + l1*2^64 + l2*2^128 + l3*2^192
    let target = limbs[0] as f64
        + (limbs[1] as f64) * 2f64.powi(64)
        + (limbs[2] as f64) * 2f64.powi(128)
        + (limbs[3] as f64) * 2f64.powi(192);
    if target <= 0.0 {
        return FALLBACK_DIFFICULTY;
    }
    // 2^256 / target
    (2f64.powi(256)) / target
}

/// Convert a fixed-size byte array (`[u8; N]`) composite value back into bytes.
/// A `[u8; N]` is decoded by scale-value as an unnamed composite of u128s, so we
/// read each element as a u128 and mask to a byte.
fn composite_to_bytes(value: &subxt::dynamic::Value, len: usize) -> [u8; 32] {
    use subxt::ext::scale_value::ValueDef;
    let mut out = [0u8; 32];
    if let ValueDef::Composite(c) = &value.value {
        for (i, v) in c.values().enumerate().take(len.min(32)) {
            out[i] = (v.as_u128().unwrap_or(0) & 0xff) as u8;
        }
    }
    out
}

/// Extract up to 4 u64 limbs from a composite value (`[u64; 4]` array/tuple).
fn composite_limbs(value: &subxt::dynamic::Value) -> [u64; 4] {
    use subxt::ext::scale_value::ValueDef;
    let mut out = [0u64; 4];
    if let ValueDef::Composite(c) = &value.value {
        for (i, v) in c.values().enumerate().take(4) {
            out[i] = v.as_u128().map(|x| x as u64).unwrap_or(0);
        }
    }
    out
}

/// Look up a named u64 field inside a scale composite value.
fn get_u64_field(value: &subxt::dynamic::Value, name: &str) -> Option<u64> {
    get_field(value, name).and_then(|v| v.as_u128()).map(|v| v as u64)
}

fn get_u32_field(value: &subxt::dynamic::Value, name: &str) -> Option<u32> {
    get_u64_field(value, name).map(|v| v as u32)
}

fn get_u128_field(value: &subxt::dynamic::Value, name: &str) -> Option<u128> {
    get_field(value, name).and_then(|v| v.as_u128())
}

/// Look up a named field inside a scale composite value (returns the sub-value).
fn get_field<'a>(value: &'a subxt::dynamic::Value, name: &str) -> Option<&'a subxt::dynamic::Value> {
    use subxt::ext::scale_value::ValueDef;
    match &value.value {
        ValueDef::Composite(c) => match c {
            subxt::ext::scale_value::Composite::Named(pairs) => {
                pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v)
            }
            subxt::ext::scale_value::Composite::Unnamed(_) => None,
        },
        _ => None,
    }
}

/// Extract the named fields of a composite value (or `None` if not composite).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_to_difficulty_matches_classic_formula() {
        // target 2^244 -> difficulty 2^12 = 4096
        // limbs little-endian: 2^244 = limb[3] = 2^(244-192) = 2^52
        let limbs = [0u64, 0, 0, 1u64 << 52];
        let d = target_to_difficulty(limbs);
        assert!((d - 4096.0).abs() < 1.0, "expected ~4096, got {}", d);

        // target 2^240 -> difficulty 2^16 = 65536
        let limbs = [0u64, 0, 0, 1u64 << 48];
        let d = target_to_difficulty(limbs);
        assert!((d - 65536.0).abs() < 1.0, "expected ~65536, got {}", d);
    }

    #[test]
    fn target_to_difficulty_zero_guarded() {
        let d = target_to_difficulty([0, 0, 0, 0]);
        assert_eq!(d, FALLBACK_DIFFICULTY);
    }

    #[test]
    fn composite_to_bytes_roundtrips_u8_array() {
        // A `[u8; 4]` decodes as an unnamed composite of u128s.
        let v = subxt::dynamic::Value::from_bytes([0xde, 0xad, 0xbe, 0xef]);
        let out = composite_to_bytes(&v, 4);
        assert_eq!(&out[..4], &[0xde, 0xad, 0xbe, 0xef]);
        // remaining bytes are zero-filled
        assert_eq!(&out[4..], &[0u8; 28]);
    }

    #[test]
    fn composite_to_bytes_pads_short_input() {
        // len > available bytes -> zero-padded tail
        let v = subxt::dynamic::Value::from_bytes([0x01u8, 0x02]);
        let out = composite_to_bytes(&v, 32);
        assert_eq!(out[0], 0x01);
        assert_eq!(out[1], 0x02);
        assert_eq!(&out[2..], &[0u8; 30]);
    }

    #[test]
    fn feed_unit_conversions() {
        // token_price_usd stored as micro-USD: 50_000 == $0.05
        let feed = OnChainFeed {
            token_price_usd: 50_000.0 / 1_000_000.0,
            avg_gpu_hour_price_usd: 2_500_000.0 / 1_000_000.0,
            utilization: 6_500.0 / 10_000.0,
            staking_apr_percent: 1_200.0 / 100.0,
            updated_at_block: 42,
        };
        assert!((feed.token_price_usd - 0.05).abs() < 1e-9);
        assert!((feed.avg_gpu_hour_price_usd - 2.5).abs() < 1e-9);
        assert!((feed.utilization - 0.65).abs() < 1e-9);
        assert!((feed.staking_apr_percent - 12.0).abs() < 1e-9);
    }

    /// LIVE verification against a running node (run manually: `cargo test
    /// -- --ignored chain::tests::live_reads_on_chain_feed`). Requires a node
    /// on ws://127.0.0.1:9944 with the profitability pallet seeded (falls back
    /// gracefully if the feed is empty).
    #[tokio::test]
    #[ignore]
    async fn live_reads_on_chain_feed() {
        let client = ChainClient::new("ws://127.0.0.1:9944").await.unwrap();
        assert!(client.client.is_some(), "expected a live subxt client");

        let difficulty = client.get_difficulty().await.unwrap();
        assert!(difficulty > 0.0, "difficulty must be positive, got {}", difficulty);
        println!("[live] difficulty = {}", difficulty);

        let token = client.get_token_price_usd().await.unwrap();
        let gpu = client.get_gpu_hour_price("RTX 4060").await.unwrap();
        let util = client.get_utilization_rate("RTX 4060").await.unwrap();
        let apr = client.get_staking_apr().await.unwrap();
        println!(
            "[live] token=${} gpu=${}/hr util={} apr={}%",
            token, gpu, util, apr
        );
    }
}
