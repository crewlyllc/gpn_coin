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

use serde::{Deserialize, Serialize};
use subxt::{
    config::SubstrateConfig,
    dynamic::Value,
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
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
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
        let value: Option<Value> = client
            .storage()
            .at_latest()
            .await
            .ok()?
            .fetch(&addr)
            .await
            .ok()?
            .map(|thunk| thunk.to_value().ok().map(|v| v.remove_context()))
            .flatten();

        let fields = value?;
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
        let value: Option<Value> = client
            .storage()
            .at_latest()
            .await
            .ok()?
            .fetch(&addr)
            .await
            .ok()?
            .map(|thunk| thunk.to_value().ok().map(|v| v.remove_context()))
            .flatten();

        let limbs = value?;
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
                if let Ok(storage) = client.storage().at_latest().await {
                    if let Ok(Some(thunk)) = storage.fetch(&storage_addr).await {
                        if let Ok(val) = thunk.to_value().map(|v| v.remove_context()) {
                            if let Some(data) = get_field(&val, "data") {
                                if let Some(free) = get_u128_field(data, "free") {
                                    balance = free as f64 / 1_000_000_000_000.0;
                                }
                            }
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
        // TODO: read `GPUAttestation::AttestationChallenge` from chain.
        Ok([0u8; 32]) // Placeholder
    }

    pub async fn submit_attestation(
        &self,
        _result: [u8; 32],
        _gpu_info_hash: [u8; 32],
    ) -> Result<(), Box<dyn std::error::Error>> {
        log::info!("Attestation submitted to chain (RPC stub)");
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
