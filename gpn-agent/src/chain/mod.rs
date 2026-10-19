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
pub mod channel;
pub mod signer;

use serde::{Deserialize, Serialize};
use subxt::{config::SubstrateConfig, dynamic::Value, tx::Signer as _, OnlineClient};

/// Fallback defaults when the chain is unreachable / feed empty. Matches the
/// previous hardcoded placeholders so behaviour is unchanged offline.
const FALLBACK_TOKEN_PRICE_USD: f64 = 0.15;
const FALLBACK_GPU_HOUR_PRICE_USD: f64 = 2.5;
const FALLBACK_UTILIZATION: f64 = 0.65;
const FALLBACK_STAKING_APR_PERCENT: f64 = 12.0;
const FALLBACK_DIFFICULTY: f64 = 100_000_000.0;

/// Parsed on-chain profitability feed (dollars / fraction / percent forms).
#[derive(Clone, Debug)]
pub struct OnChainFeed {
    pub token_price_usd: f64,
    pub avg_gpu_hour_price_usd: f64,
    pub utilization: f64,         // 0.0 ..= 1.0
    pub staking_apr_percent: f64, // e.g. 12.0 == 12%
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
    /// Lazily-initialized, process-shared inclusion-free submitter used to
    /// pipeline marketplace lifecycle txs (accept→start→submit_result→complete)
    /// without waiting on block time. See [`signer::NonceStream`].
    fast: std::sync::Arc<tokio::sync::Mutex<Option<signer::NonceStream>>>,
}

/// A dropped/closed RPC socket, not a chain-level rejection. jsonrpsee reports a
/// dead background task as `RestartNeeded` ("The background task closed ...;
/// restart required"); other variants say the connection closed / is not
/// connected. None of these mean the transaction was rejected — the submit never
/// reached the node — so a fresh connection may safely resubmit the same call.
fn is_connection_lost(msg: &str) -> bool {
    let m = msg.to_lowercase();
    m.contains("restart required")
        || m.contains("background task closed")
        || m.contains("connection closed")
        || m.contains("connection reset")
        || m.contains("closed connection")
        || m.contains("not connected")
        || m.contains("disconnected")
        || m.contains("broken pipe")
}

/// Resolve the readiness-gate timeout/poll from the environment and wait for the
/// node RPC to report a live best block. Bounded and non-fatal — on timeout it
/// returns the last [`signer::NodeReadiness`] so the caller can log and proceed
/// (the agent is designed to keep working even when the chain is down).
async fn gate_rpc_readiness(endpoint: &str) -> signer::NodeReadiness {
    let enabled = std::env::var("GPN_RPC_READY_GATE")
        .map(|v| !(v == "0" || v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("off")))
        .unwrap_or(true);
    if !enabled {
        return signer::NodeReadiness {
            reachable: true,
            head_block: 1,
            peers: 0,
            is_syncing: false,
            should_have_peers: false,
        };
    }
    let timeout_ms: u64 = std::env::var("GPN_RPC_READY_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(15_000);
    signer::wait_until_ready(
        endpoint,
        std::time::Duration::from_millis(timeout_ms),
        std::time::Duration::from_millis(500),
    )
    .await
}

impl ChainClient {
    /// Connect to the GPN chain. On failure we log a warning and keep
    /// `client = None` so all reads fall back to placeholder defaults — the
    /// agent must keep working even when the chain is down.
    pub async fn new(endpoint: &str) -> Result<Self, Box<dyn std::error::Error>> {
        log::info!("Connecting to GPN chain at {}", endpoint);
        // Use the robust connection path that tolerates a pruned finalized
        // block on PoW chains without a finality gadget (see
        // `signer::connect_robust`).
        let client = match signer::connect_robust::<SubstrateConfig>(endpoint).await {
            Ok(c) => {
                log::info!("Connected to GPN chain (subxt) at {}", endpoint);
                Some(c)
            }
            Err(e) => {
                log::warn!(
                    "Could not connect to GPN chain at {} ({}). \
                     Falling back to placeholder chain data.",
                    endpoint,
                    e
                );
                None
            }
        };
        // RPC-readiness gate (queue item 9c): a long-lived client must not start
        // submitting lifecycle txs against a node that is still booting or not
        // yet producing blocks. `connect_robust` can succeed against a node that
        // is up but still at genesis / mid-sync, in which case reads return
        // genesis-era state and dynamically-encoded calls get dropped or diverge
        // the nonce stream. Wait (bounded, non-fatal) for a live best block
        // before proceeding. Opt out with GPN_RPC_READY_GATE=0; tune with
        // GPN_RPC_READY_TIMEOUT_MS (default 15000).
        let ready = gate_rpc_readiness(endpoint).await;
        if ready.is_ready() {
            log::info!(
                "RPC readiness gate: node ready (head #{}, peers {}, syncing {})",
                ready.head_block,
                ready.peers,
                ready.is_syncing
            );
        } else {
            log::warn!(
                "RPC readiness gate: node not ready (reachable={}, head #{}, peers {}, syncing {}); proceeding anyway",
                ready.reachable,
                ready.head_block,
                ready.peers,
                ready.is_syncing
            );
        }
        Ok(Self {
            endpoint: endpoint.to_string(),
            client,
            signer_uri: None,
            fast: Default::default(),
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
    async fn storage_at_head<Addr>(
        &self,
        client: &OnlineClient<SubstrateConfig>,
        addr: &Addr,
    ) -> Option<subxt::dynamic::Value>
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
                .and_then(|t| t.to_value().ok().map(|v| v.remove_context())),
            None => client
                .storage()
                .at_latest()
                .await
                .ok()?
                .fetch(addr)
                .await
                .ok()?
                .and_then(|t| t.to_value().ok().map(|v| v.remove_context())),
        }
    }

    /// Read the current best (head) block **number** via `chain_getHeader`
    /// (JSON-RPC, ws->http). Needed to judge feed staleness (the pallet stores
    /// `updated_at` as the block number the feed was written at). Returns
    /// `None` if the header can't be read.
    pub async fn head_block_number(&self) -> Option<u64> {
        let http_url = self
            .endpoint
            .replace("ws://", "http://")
            .replace("wss://", "https://");
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "chain_getHeader",
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
        // Header.number is a 0x-prefixed hex string.
        let number_hex = resp.get("result")?.get("number")?.as_str()?;
        u64::from_str_radix(number_hex.trim_start_matches("0x"), 16).ok()
    }

    /// Public access to the parsed on-chain profitability feed (dollars /
    /// fraction / percent forms + `updated_at_block`). Returns `None` if the
    /// chain is unreachable or the feed has not been set by an operator yet.
    pub async fn get_feed(&self) -> Option<OnChainFeed> {
        self.fetch_feed().await
    }

    /// Read the on-chain profitability feed. Returns `None` if the chain is
    /// unreachable or the feed has not been set by an oracle operator yet.
    async fn fetch_feed(&self) -> Option<OnChainFeed> {
        let client = self.client.as_ref()?;
        let addr = subxt::dynamic::storage("ProfitabilityOracle", "CurrentFeed", vec![]);
        let fields = self.storage_at_head(client, &addr).await?;
        parse_feed_value(&fields)
    }

    /// Enumerate every oracle operator's **latest** feed (from the new
    /// `OperatorFeeds` map — populated on each `update_feed`), as
    /// `(hex account, OnChainFeed)`. Operators who have never posted a feed
    /// are absent from the map and thus skipped. Returns an empty vec if the
    /// chain is unreachable or no operator has posted yet. This is the data
    /// source for oracle-governance §6 gap #3 (client-side median across
    /// operators): consumers can aggregate all operators' feeds (e.g. take the
    /// median) so one outlier operator cannot unilaterally move the effective
    /// feed.
    pub async fn get_operator_feeds(&self) -> Vec<(String, OnChainFeed)> {
        let client = match self.client.as_ref() {
            Some(c) => c,
            None => return Vec::new(),
        };
        let addr = subxt::dynamic::storage("ProfitabilityOracle", "OperatorFeeds", vec![]);
        let mut out = Vec::new();
        if let Some(hash) = self.head_block_hash().await {
            if let Ok(mut stream) = client.storage().at(hash).iter(addr).await {
                while let Some(Ok(kv)) = stream.next().await {
                    let op = kv
                        .keys
                        .first()
                        .map(account_id_to_string)
                        .unwrap_or_default();
                    if let Ok(v) = kv.value.to_value() {
                        if let Some(feed) = parse_feed_value(&v.remove_context()) {
                            out.push((op, feed));
                        }
                    }
                }
            }
        }
        out
    }

    /// Read a bonded (permissionless) oracle operator's bond size (Phase C,
    /// §6 gap #5) from `ProfitabilityOracle::StakedOperators`. Returns `None`
    /// if the account is not bonded or the chain is unreachable.
    pub async fn get_staked_bond(&self, account: &[u8; 32]) -> Option<u128> {
        let client = self.client.as_ref()?;
        let addr = subxt::dynamic::storage(
            "ProfitabilityOracle",
            "StakedOperators",
            vec![Value::from_bytes(account)],
        );
        self.storage_at_head(client, &addr).await?.as_u128()
    }

    /// Enumerate every **bonded** (permissionless, Phase-C) oracle operator's
    /// bond size from `ProfitabilityOracle::StakedOperators`, as
    /// `(hex account, bond GPN-raw)`. This is the bond-weight side of the
    /// bond-weighted median aggregation: paired with `get_operator_feeds()`, a
    /// consumer can weight each operator's latest feed by how much GPN they
    /// bonded, so a large-bond operator dominates the effective feed and a
    /// tiny-bond operator (even an outlier) cannot move it. Returns an empty
    /// vec if the chain is unreachable or no operator has bonded.
    pub async fn get_staked_bonds(&self) -> Vec<(String, u128)> {
        let client = match self.client.as_ref() {
            Some(c) => c,
            None => return Vec::new(),
        };
        let addr = subxt::dynamic::storage("ProfitabilityOracle", "StakedOperators", vec![]);
        let mut out = Vec::new();
        if let Some(hash) = self.head_block_hash().await {
            if let Ok(mut stream) = client.storage().at(hash).iter(addr).await {
                while let Some(Ok(kv)) = stream.next().await {
                    let op = kv
                        .keys
                        .first()
                        .map(account_id_to_string)
                        .unwrap_or_default();
                    if let Ok(v) = kv.value.to_value() {
                        if let Some(bond) = v.remove_context().as_u128() {
                            out.push((op, bond));
                        }
                    }
                }
            }
        }
        out
    }

    /// Read the **on-chain** Phase-C bond-weighted median profitability feed
    /// via the `gpn_effectiveFeed` RPC (a JSON-RPC POST over ws->http, same
    /// pattern as `head_block_hash`). This is the authoritative aggregation the
    /// pallet computes (`ProfitabilityOracle::effective_feed()`): a consumer
    /// can read the on-chain bond-weighted median directly instead of
    /// reimplementing it client-side. Returns `None` if no bonded operator has
    /// posted a feed yet, or if the chain/RPC is unreachable.
    pub async fn get_effective_feed(&self) -> Option<OnChainFeed> {
        let http_url = self
            .endpoint
            .replace("ws://", "http://")
            .replace("wss://", "https://");
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "gpn_effectiveFeed",
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
        let res = resp.get("result")?;
        if res.is_null() {
            return None;
        }
        // Convenience fields are already decoded USD (not micro-USD); the raw
        // fields are micro-USD / bps like the pallet.
        Some(OnChainFeed {
            token_price_usd: res.get("tokenPrice")?.as_f64()?.max(0.0),
            avg_gpu_hour_price_usd: res.get("gpuHourPriceUsd")?.as_f64()?.max(0.0),
            utilization: res.get("utilization")?.as_f64()?.clamp(0.0, 1.0),
            staking_apr_percent: res.get("stakingApr")?.as_f64()? * 100.0,
            updated_at_block: res.get("updatedAt")?.as_u64()?,
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

    pub async fn get_account_info(
        &self,
        address: &str,
    ) -> Result<AccountInfo, Box<dyn std::error::Error>> {
        // Best-effort: read System::Account balance if connected, else 0.
        let mut balance = 0.0f64;
        if let Some(client) = &self.client {
            if let Ok(addr32) = address.parse::<subxt::utils::AccountId32>() {
                let storage_addr = subxt::dynamic::storage(
                    "System",
                    "Account",
                    vec![Value::from_bytes(addr32.as_ref() as &[u8; 32])],
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
        Ok(self
            .feed_value(|f| f.token_price_usd, FALLBACK_TOKEN_PRICE_USD)
            .await)
    }

    pub async fn get_gpu_hour_price(
        &self,
        _model: &str,
    ) -> Result<f64, Box<dyn std::error::Error>> {
        Ok(self
            .feed_value(|f| f.avg_gpu_hour_price_usd, FALLBACK_GPU_HOUR_PRICE_USD)
            .await)
    }

    pub async fn get_utilization_rate(
        &self,
        _model: &str,
    ) -> Result<f64, Box<dyn std::error::Error>> {
        Ok(self
            .feed_value(|f| f.utilization, FALLBACK_UTILIZATION)
            .await)
    }

    pub async fn get_staking_apr(&self) -> Result<f64, Box<dyn std::error::Error>> {
        Ok(self
            .feed_value(|f| f.staking_apr_percent, FALLBACK_STAKING_APR_PERCENT)
            .await)
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

    /// Read the account's current on-chain nonce from the **head** block.
    /// Returns 0 if the chain is unreachable / account not found.
    ///
    /// Critical for signing: subxt's default nonce comes from the *finalized*
    /// block, which on a PoW dev chain is stuck at #0. Without an explicit
    /// head-block nonce, a second+ signed tx from the same account is rejected
    /// as `Transaction is outdated`.
    pub async fn get_account_nonce(&self, address: &str) -> u64 {
        let Some(client) = &self.client else { return 0 };
        let Ok(addr32) = address.parse::<subxt::utils::AccountId32>() else {
            return 0;
        };
        let storage_addr = subxt::dynamic::storage(
            "System",
            "Account",
            vec![Value::from_bytes(addr32.as_ref() as &[u8; 32])],
        );
        match self.storage_at_head(client, &storage_addr).await {
            Some(v) => get_u32_field(&v, "nonce").unwrap_or(0) as u64,
            None => 0,
        }
    }

    /// Read whether the account has staked (has a `ProviderStake` entry) and
    /// whether it has an active attestation record, straight from storage.
    pub async fn get_provider_state(&self, address: &str) -> (bool, bool) {
        let Some(client) = &self.client else {
            return (false, false);
        };
        let Ok(addr32) = address.parse::<subxt::utils::AccountId32>() else {
            return (false, false);
        };
        let key = (addr32.as_ref() as &[u8; 32]).to_vec();
        let stake = subxt::dynamic::storage(
            "GPUAttestation",
            "ProviderStake",
            vec![Value::from_bytes(key.clone())],
        );
        let att = subxt::dynamic::storage(
            "GPUAttestation",
            "Attestations",
            vec![Value::from_bytes(key)],
        );
        let has_stake = self.storage_at_head(client, &stake).await.is_some();
        let has_att = self.storage_at_head(client, &att).await.is_some();
        (has_stake, has_att)
    }

    /// Read the provider's attestation record: (gpu_count, compute_gflops,
    /// tier). Returns None if not attested / record unreadable.
    pub async fn get_attestation_record(&self, address: &str) -> Option<(u8, u64, String)> {
        let client = self.client.as_ref()?;
        let addr32 = address.parse::<subxt::utils::AccountId32>().ok()?;
        let key = (addr32.as_ref() as &[u8; 32]).to_vec();
        let att = subxt::dynamic::storage(
            "GPUAttestation",
            "Attestations",
            vec![Value::from_bytes(key)],
        );
        let v = self.storage_at_head(client, &att).await?;
        let gpus = get_u8_field(&v, "gpu_count")?;
        let gflops = get_u64_field(&v, "compute_gflops")?;
        let tier = get_field(&v, "tier")
            .and_then(|t| {
                // AttestationTier enum decodes as a named variant (e.g. { Bronze: null }).
                use subxt::ext::scale_value::ValueDef;
                match &t.value {
                    ValueDef::Variant(v) => Some(v.name.clone()),
                    ValueDef::Composite(subxt::ext::scale_value::Composite::Named(pairs)) => {
                        pairs.first().map(|(name, _)| name.clone())
                    }
                    _ => t.as_u128().map(|n| {
                        match n {
                            1 => "Bronze",
                            2 => "Silver",
                            3 => "Gold",
                            _ => "Unattested",
                        }
                        .to_string()
                    }),
                }
            })
            .unwrap_or_else(|| "?".to_string());
        Some((gpus, gflops, tier))
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

        let nonce = self
            .get_account_nonce(&signer.account_id().to_string())
            .await;
        signer::submit_signed(&client, &call, &signer, nonce).await?;
        log::info!("GPU attestation submitted on-chain and succeeded.");
        Ok(())
    }

    /// Ensure the provider has staked at least `MinAttestationStake` so that
    /// `submit_attestation` is accepted by the pallet (which rejects
    /// un-staked providers with `InsufficientStake`). Deposits `amount` via the
    /// signed `GPUAttestation::deposit_stake` extrinsic.
    ///
    /// This is a no-op (with a warning) if no signer is configured.
    pub async fn deposit_stake(&self, amount: u128) -> Result<(), Box<dyn std::error::Error>> {
        let Some(uri) = &self.signer_uri else {
            log::warn!("No signer configured; cannot deposit stake (set signer_uri).");
            return Ok(());
        };

        let signer = signer::Sr25519Signer::from_uri(uri)?;
        let client = signer::connect(&self.endpoint).await?;

        log::info!(
            "Depositing {} GPN stake as {} (GPUAttestation::deposit_stake)",
            amount,
            signer.account_id()
        );

        let nonce = self
            .get_account_nonce(&signer.account_id().to_string())
            .await;
        let call = subxt::dynamic::tx("GPUAttestation", "deposit_stake", vec![Value::u128(amount)]);

        signer::submit_signed(&client, &call, &signer, nonce).await?;
        log::info!("Stake deposited on-chain.");
        Ok(())
    }

    /// Withdraw the provider's stake (removes the attestation record, so the
    /// provider is no longer attested and cannot take gated jobs). Signed
    /// `GPUAttestation::withdraw_stake` extrinsic.
    pub async fn withdraw_stake(&self) -> Result<(), Box<dyn std::error::Error>> {
        let Some(uri) = &self.signer_uri else {
            log::warn!("No signer configured; cannot withdraw stake (set signer_uri).");
            return Ok(());
        };

        let signer = signer::Sr25519Signer::from_uri(uri)?;
        let client = signer::connect(&self.endpoint).await?;

        log::info!(
            "Withdrawing stake as {} (GPUAttestation::withdraw_stake)",
            signer.account_id()
        );

        let nonce = self
            .get_account_nonce(&signer.account_id().to_string())
            .await;
        let call_args: Vec<subxt::dynamic::Value> = Vec::new();
        let call = subxt::dynamic::tx("GPUAttestation", "withdraw_stake", call_args);

        signer::submit_signed(&client, &call, &signer, nonce).await?;
        log::info!("Stake withdrawn on-chain.");
        Ok(())
    }

    // ---- Compute marketplace extrinsics (signed, dynamic) ----------------

    /// Build a dynamic `GPURequirements` composite value (named fields).
    fn gpu_requirements_value(
        min_vram_mb: u32,
        min_compute_capability: u16,
        min_gpu_count: u8,
        min_cuda_cores: u32,
        min_gflops: u64,
    ) -> Value {
        Value::named_composite([
            ("min_vram_mb", Value::u128(min_vram_mb as u128)),
            (
                "min_compute_capability",
                Value::u128(min_compute_capability as u128),
            ),
            ("min_gpu_count", Value::u128(min_gpu_count as u128)),
            ("min_cuda_cores", Value::u128(min_cuda_cores as u128)),
            ("min_gflops", Value::u128(min_gflops as u128)),
        ])
    }

    async fn submit_signed_call(
        &self,
        pallet: &str,
        call_name: &str,
        args: Vec<Value>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(uri) = &self.signer_uri else {
            log::warn!(
                "No signer configured; cannot submit {}::{}. (set signer_uri)",
                pallet,
                call_name
            );
            return Ok(());
        };
        let signer = signer::Sr25519Signer::from_uri(uri)?;
        let client = signer::connect(&self.endpoint).await?;
        let call = subxt::dynamic::tx(pallet, call_name, args);
        let nonce = self
            .get_account_nonce(&signer.account_id().to_string())
            .await;
        log::info!(
            "Submitting {}::{} as {}",
            pallet,
            call_name,
            signer.account_id()
        );
        signer::submit_signed(&client, &call, &signer, nonce).await?;
        log::info!("{}::{} succeeded on-chain.", pallet, call_name);
        Ok(())
    }

    /// `ComputeMarketplace::register_provider(requirements)` — advertise GPU
    /// capability to the marketplace. Requires no stake, but the provider must
    /// be attested to accept gated (`min_gflops > 0`) jobs.
    pub async fn register_provider(
        &self,
        min_vram_mb: u32,
        min_compute_capability: u16,
        min_gpu_count: u8,
        min_cuda_cores: u32,
        min_gflops: u64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let req = Self::gpu_requirements_value(
            min_vram_mb,
            min_compute_capability,
            min_gpu_count,
            min_cuda_cores,
            min_gflops,
        );
        self.submit_signed_call("ComputeMarketplace", "register_provider", vec![req])
            .await
    }

    /// `ComputeMarketplace::set_provider_endpoint(endpoint)` — advertise this
    /// provider's reachable `/status` URL on-chain so access points / the
    /// scheduler can DISCOVER providers from the chain instead of a static list
    /// (item 1: on-chain provider discovery). The signer must already be a
    /// registered provider; the endpoint is capped at 256 bytes on-chain.
    pub async fn set_provider_endpoint(
        &self,
        endpoint: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.submit_signed_call(
            "ComputeMarketplace",
            "set_provider_endpoint",
            vec![Value::from_bytes(endpoint.as_bytes())],
        )
        .await
    }

    /// `ComputeMarketplace::open_channel(provider, amount, ttl)` — lock `amount`
    /// of escrow on-chain for an off-chain payment channel to `provider`. The
    /// chain sees only this open (+ the later close); per-job payments stay
    /// off-chain (item #2, `design-offchain-escrow-channels.md`).
    pub async fn open_channel(
        &self,
        provider: [u8; 32],
        amount: u128,
        ttl: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.submit_signed_call(
            "ComputeMarketplace",
            "open_channel",
            vec![
                Value::from_bytes(&provider),
                Value::u128(amount),
                Value::u128(ttl as u128),
            ],
        )
        .await
    }

    /// `ComputeMarketplace::close_channel(...)` — cooperative close authorised by
    /// BOTH parties' off-chain settle signatures (see `chain::channel`).
    #[allow(clippy::too_many_arguments)]
    pub async fn close_channel(
        &self,
        channel_id: u64,
        balance_requestor: u128,
        balance_provider: u128,
        nonce: u64,
        sig_requestor: [u8; 64],
        sig_provider: [u8; 64],
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.submit_signed_call(
            "ComputeMarketplace",
            "close_channel",
            vec![
                Value::u128(channel_id as u128),
                Value::u128(balance_requestor),
                Value::u128(balance_provider),
                Value::u128(nonce as u128),
                Value::from_bytes(&sig_requestor),
                Value::from_bytes(&sig_provider),
            ],
        )
        .await
    }

    /// `ComputeMarketplace::force_close_channel(...)` — after the channel TTL, a
    /// party settles with the **counterparty's** latest signed state (its own
    /// authorization is implicit). Nonce monotonicity blocks replaying a stale
    /// self-favoring state.
    pub async fn force_close_channel(
        &self,
        channel_id: u64,
        balance_requestor: u128,
        balance_provider: u128,
        nonce: u64,
        sig_counterparty: [u8; 64],
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.submit_signed_call(
            "ComputeMarketplace",
            "force_close_channel",
            vec![
                Value::u128(channel_id as u128),
                Value::u128(balance_requestor),
                Value::u128(balance_provider),
                Value::u128(nonce as u128),
                Value::from_bytes(&sig_counterparty),
            ],
        )
        .await
    }

    /// `ComputeMarketplace::create_job(...)` — a requestor posts a paid AI job
    /// (escrow transferred to the marketplace account on creation).
    #[allow(clippy::too_many_arguments)]
    pub async fn create_job(
        &self,
        min_vram_mb: u32,
        min_compute_capability: u16,
        min_gpu_count: u8,
        min_cuda_cores: u32,
        min_gflops: u64,
        job_type: &str,
        tier: &str,
        price: u128,
        price_per_token: u128,
        max_tokens: u32,
        max_duration: u32,
        workload_hash: [u8; 32],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let req = Self::gpu_requirements_value(
            min_vram_mb,
            min_compute_capability,
            min_gpu_count,
            min_cuda_cores,
            min_gflops,
        );
        let args = vec![
            req,
            Value::variant(
                job_type,
                subxt::ext::scale_value::Composite::unnamed(Vec::<Value>::new()),
            ),
            Value::variant(
                tier,
                subxt::ext::scale_value::Composite::unnamed(Vec::<Value>::new()),
            ),
            Value::u128(price),
            Value::u128(price_per_token),
            Value::u128(max_tokens as u128),
            Value::u128(max_duration as u128),
            Value::from_bytes(workload_hash),
        ];
        self.submit_signed_call("ComputeMarketplace", "create_job", args)
            .await
    }

    /// Inclusion-free `create_job` for the access-point fast path. Returns as
    /// soon as the tx is accepted into the pool; the caller predicts the new job
    /// id from `get_next_job_id()` (the pallet assigns `id = NextJobId` then
    /// increments, so the new id equals the counter read just before submit).
    ///
    /// Note: unlike the claim path, `create_job` is signed by the *access-point*
    /// account while `accept_job` is signed by the *provider* account, so the tx
    /// pool cannot order them relative to each other. The provider therefore
    /// tolerates a not-yet-on-chain job (its accept may land first and fail
    /// dispatch; the next poll re-drives it) — see `gpn_llm_provider`.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_job_fast(
        &self,
        min_vram_mb: u32,
        min_compute_capability: u16,
        min_gpu_count: u8,
        min_cuda_cores: u32,
        min_gflops: u64,
        job_type: &str,
        tier: &str,
        price: u128,
        price_per_token: u128,
        max_tokens: u32,
        max_duration: u32,
        workload_hash: [u8; 32],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let req = Self::gpu_requirements_value(
            min_vram_mb,
            min_compute_capability,
            min_gpu_count,
            min_cuda_cores,
            min_gflops,
        );
        let args = vec![
            req,
            Value::variant(
                job_type,
                subxt::ext::scale_value::Composite::unnamed(Vec::<Value>::new()),
            ),
            Value::variant(
                tier,
                subxt::ext::scale_value::Composite::unnamed(Vec::<Value>::new()),
            ),
            Value::u128(price),
            Value::u128(price_per_token),
            Value::u128(max_tokens as u128),
            Value::u128(max_duration as u128),
            Value::from_bytes(workload_hash),
        ];
        self.submit_fast("ComputeMarketplace", "create_job", args)
            .await
    }

    /// `ComputeMarketplace::accept_job(job_id)` — provider takes a Pending job.
    /// Gated by the cycle-26 cross-pallet `min_gflops` attestation check.
    pub async fn accept_job(&self, job_id: u64) -> Result<(), Box<dyn std::error::Error>> {
        self.submit_signed_call(
            "ComputeMarketplace",
            "accept_job",
            vec![Value::u128(job_id as u128)],
        )
        .await
    }

    /// `ComputeMarketplace::start_job(job_id)` — mark an Accepted job Running.
    pub async fn start_job(&self, job_id: u64) -> Result<(), Box<dyn std::error::Error>> {
        self.submit_signed_call(
            "ComputeMarketplace",
            "start_job",
            vec![Value::u128(job_id as u128)],
        )
        .await
    }

    /// `ComputeMarketplace::submit_result(job_id, result_hash)` — provider
    /// reports the digest of the computed workload; state → AwaitingVerification.
    pub async fn submit_result(
        &self,
        job_id: u64,
        result_hash: [u8; 32],
        actual_tokens: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.submit_signed_call(
            "ComputeMarketplace",
            "submit_result",
            vec![
                Value::u128(job_id as u128),
                Value::from_bytes(result_hash),
                Value::u128(actual_tokens as u128),
            ],
        )
        .await
    }

    /// `ComputeMarketplace::complete_job(job_id)` — finalize and pay the
    /// provider from escrow (marketplace fee withheld).
    pub async fn complete_job(&self, job_id: u64) -> Result<(), Box<dyn std::error::Error>> {
        self.submit_signed_call(
            "ComputeMarketplace",
            "complete_job",
            vec![Value::u128(job_id as u128)],
        )
        .await
    }

    /// Get-or-create the process-shared, inclusion-free nonce stream, then
    /// submit `pallet::call(args)` with the next nonce. Never waits for a
    /// block. On a failed submit the stream is torn down so the next call
    /// reconnects and re-seeds its nonce from the chain.
    pub async fn submit_fast(
        &self,
        pallet: &str,
        call_name: &str,
        args: Vec<Value>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.submit_fast_nonce(pallet, call_name, args)
            .await
            .map(|_| ())
    }

    /// Same as [`ChainClient::submit_fast`], but returns the **nonce** the tx was
    /// signed with. Callers can later compare it against the account nonce read
    /// at a later head: a strictly greater account nonce is proof that this tx
    /// was included (nonces are ordered and never skipped), which is the only
    /// exact evidence that a pool-ack submit actually landed.
    pub async fn submit_fast_nonce(
        &self,
        pallet: &str,
        call_name: &str,
        args: Vec<Value>,
    ) -> Result<u64, Box<dyn std::error::Error>> {
        let mut guard = self.fast.lock().await;
        // Runtime-upgrade resilience: if the chain's spec_version changed since the
        // stream connected, its metadata is stale → post-upgrade calls get dropped and
        // the pipelined nonce stream silently diverges (the create-path stall). Drop the
        // stream so we reconnect with fresh best-block metadata + a re-seeded nonce.
        if let Some(cur) = signer::runtime_spec_version(&self.endpoint).await {
            if guard.as_ref().map_or(false, |s| s.spec_version() != cur) {
                log::warn!(
                    "runtime spec changed ({} -> {}) — re-seeding the fast submitter",
                    guard.as_ref().map(|s| s.spec_version()).unwrap_or(0),
                    cur
                );
                *guard = None;
            }
        }
        // One bounded reconnect retry. The cached stream holds a long-lived WS
        // client; if it idles long enough for the node / RPC server to close the
        // socket, jsonrpsee's background task ends and the NEXT submit fails with
        // `RestartNeeded` ("The background task closed ...; restart required")
        // even though the chain is healthy. Previously that surfaced to the user
        // as a failed request and only the following request (which re-created the
        // stream) succeeded. Treat it as "our socket is stale": rebuild once and
        // resubmit. Bounded to a single extra attempt, still pool-ack only.
        let mut reconnected = false;
        loop {
            if guard.is_none() {
                let Some(uri) = &self.signer_uri else {
                    return Err(format!(
                        "No signer configured; cannot submit {}::{}.",
                        pallet, call_name
                    )
                    .into());
                };
                *guard = Some(signer::NonceStream::new(&self.endpoint, uri).await?);
            }
            let stream = guard.as_mut().expect("fast stream initialized above");
            match stream.submit(pallet, call_name, args.clone()).await {
                Ok(nonce) => {
                    log::info!(
                        "{}::{} accepted into pool at nonce {} (no inclusion wait)",
                        pallet,
                        call_name,
                        nonce
                    );
                    return Ok(nonce);
                }
                Err(e) => {
                    // Drop the stream: its local nonce may now be ahead of / behind
                    // the chain. The next submit_fast reconnects and re-seeds.
                    *guard = None;
                    if !reconnected && is_connection_lost(&e.to_string()) {
                        reconnected = true;
                        log::warn!(
                            "fast submit {}::{} lost its RPC connection ({}) — reconnecting once",
                            pallet,
                            call_name,
                            e
                        );
                        println!(
                            "[submit_fast] {pallet}::{call_name} RPC connection lost — reconnecting once"
                        );
                        continue;
                    }
                    return Err(e);
                }
            }
        }
    }

    /// Pipeline the two-tx claim (`accept_job` → `start_job`) as a single
    /// nonce-ordered burst with **no block wait**. One connection, one nonce
    /// read, two back-to-back pool accepts. This replaces the old
    /// accept-then-wait-for-a-block-then-start sequence (~2 blocks ≈ 8–14s)
    /// with ~two pool acks (~ms).
    pub async fn claim_job_pipelined(&self, job_id: u64) -> Result<(), Box<dyn std::error::Error>> {
        self.submit_fast(
            "ComputeMarketplace",
            "accept_job",
            vec![Value::u128(job_id as u128)],
        )
        .await?;
        self.submit_fast(
            "ComputeMarketplace",
            "start_job",
            vec![Value::u128(job_id as u128)],
        )
        .await?;
        Ok(())
    }

    /// Inclusion-free `cancel_job` for the janitor/self-heal path (shared nonce
    /// stream). The requestor-only + Pending-only guards are enforced on-chain.
    pub async fn cancel_job_fast(&self, job_id: u64) -> Result<(), Box<dyn std::error::Error>> {
        self.submit_fast(
            "ComputeMarketplace",
            "cancel_job",
            vec![Value::u128(job_id as u128)],
        )
        .await
    }

    /// Inclusion-free `accept_job` for the pipelined/self-heal path.
    pub async fn accept_job_fast(&self, job_id: u64) -> Result<(), Box<dyn std::error::Error>> {
        self.submit_fast(
            "ComputeMarketplace",
            "accept_job",
            vec![Value::u128(job_id as u128)],
        )
        .await
    }

    /// Inclusion-free `start_job` for the pipelined/self-heal path.
    pub async fn start_job_fast(&self, job_id: u64) -> Result<(), Box<dyn std::error::Error>> {
        self.submit_fast(
            "ComputeMarketplace",
            "start_job",
            vec![Value::u128(job_id as u128)],
        )
        .await
    }

    /// Inclusion-free `submit_result` for the pipelined serve path.
    pub async fn submit_result_fast(
        &self,
        job_id: u64,
        result_hash: [u8; 32],
        actual_tokens: u32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.submit_fast(
            "ComputeMarketplace",
            "submit_result",
            vec![
                Value::u128(job_id as u128),
                Value::from_bytes(result_hash),
                Value::u128(actual_tokens as u128),
            ],
        )
        .await
    }

    /// Inclusion-free `complete_job` for the pipelined serve path.
    pub async fn complete_job_fast(&self, job_id: u64) -> Result<(), Box<dyn std::error::Error>> {
        self.submit_fast(
            "ComputeMarketplace",
            "complete_job",
            vec![Value::u128(job_id as u128)],
        )
        .await
    }

    /// Nonce-returning variants used by the provider's heal gate. The heal gate
    /// needs the submitted nonce as inclusion evidence; every other caller keeps
    /// the plain `Result<()>` shape above.
    pub async fn accept_job_fast_nonce(
        &self,
        job_id: u64,
    ) -> Result<u64, Box<dyn std::error::Error>> {
        self.submit_fast_nonce(
            "ComputeMarketplace",
            "accept_job",
            vec![Value::u128(job_id as u128)],
        )
        .await
    }

    pub async fn start_job_fast_nonce(
        &self,
        job_id: u64,
    ) -> Result<u64, Box<dyn std::error::Error>> {
        self.submit_fast_nonce(
            "ComputeMarketplace",
            "start_job",
            vec![Value::u128(job_id as u128)],
        )
        .await
    }

    pub async fn submit_result_fast_nonce(
        &self,
        job_id: u64,
        result_hash: [u8; 32],
        actual_tokens: u32,
    ) -> Result<u64, Box<dyn std::error::Error>> {
        self.submit_fast_nonce(
            "ComputeMarketplace",
            "submit_result",
            vec![
                Value::u128(job_id as u128),
                Value::from_bytes(result_hash),
                Value::u128(actual_tokens as u128),
            ],
        )
        .await
    }

    pub async fn complete_job_fast_nonce(
        &self,
        job_id: u64,
    ) -> Result<u64, Box<dyn std::error::Error>> {
        self.submit_fast_nonce(
            "ComputeMarketplace",
            "complete_job",
            vec![Value::u128(job_id as u128)],
        )
        .await
    }

    /// Account nonce at the best head, or `None` when it cannot be read. Unlike
    /// [`ChainClient::get_account_nonce`] (which folds failures into `0`), this
    /// preserves the distinction — the heal gate must never mistake an
    /// unreadable nonce for evidence that a tx was included.
    pub async fn account_nonce_at_head(&self, address: &str) -> Option<u64> {
        let client = self.client.as_ref()?;
        let addr32 = address.parse::<subxt::utils::AccountId32>().ok()?;
        let storage_addr = subxt::dynamic::storage(
            "System",
            "Account",
            vec![Value::from_bytes(addr32.as_ref() as &[u8; 32])],
        );
        let v = self.storage_at_head(client, &storage_addr).await?;
        get_u32_field(&v, "nonce").map(|n| n as u64)
    }

    /// `ComputeMarketplace::cancel_job(job_id)` — the **requestor** cancels a
    /// still-`Pending` job. Refunds escrow and decrements the global
    /// `ActiveJobCount`, freeing a `MaxActiveJobs` slot. Used to drain orphaned
    /// Pending jobs that would otherwise saturate job creation.
    pub async fn cancel_job(&self, job_id: u64) -> Result<(), Box<dyn std::error::Error>> {
        self.submit_signed_call(
            "ComputeMarketplace",
            "cancel_job",
            vec![Value::u128(job_id as u128)],
        )
        .await
    }

    /// Read an account's ss58/hex address string for the configured signer
    /// (empty if no signer configured).
    pub fn signer_address(&self) -> Option<String> {
        let uri = self.signer_uri.as_ref()?;
        let s = signer::Sr25519Signer::from_uri(uri).ok()?;
        use subxt::tx::Signer as _;
        Some(s.account_id().to_string())
    }

    /// Signer address as lowercase `0x…` hex — the exact format `get_job_state`
    /// returns for `requestor`, so callers can match "is this job mine?".
    pub fn signer_address_hex(&self) -> Option<String> {
        let uri = self.signer_uri.as_ref()?;
        let s = signer::Sr25519Signer::from_uri(uri).ok()?;
        use subxt::tx::Signer as _;
        Some(format!("0x{}", hex::encode(s.account_id().0)))
    }

    /// `ComputeMarketplace::fail_job(job_id, reason)` — mark a non-terminal job
    /// Failed and refund the escrow immediately. Callable by the assigned provider
    /// or the requestor. Frees the global `ActiveJobCount` slot.
    pub async fn fail_job(
        &self,
        job_id: u64,
        reason: u8,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.submit_signed_call(
            "ComputeMarketplace",
            "fail_job",
            vec![Value::u128(job_id as u128), Value::u128(reason as u128)],
        )
        .await
    }

    /// Inclusion-free `fail_job` for the shared nonce stream.
    pub async fn fail_job_fast(
        &self,
        job_id: u64,
        reason: u8,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.submit_fast(
            "ComputeMarketplace",
            "fail_job",
            vec![Value::u128(job_id as u128), Value::u128(reason as u128)],
        )
        .await
    }

    /// `ComputeMarketplace::expire_job(job_id)` — **permissionless** on-chain
    /// reclaim of a non-terminal job that outlived its deadline
    /// (`now > created_at + max_duration`). Unlike `cancel_job` (requestor-only,
    /// Pending-only) this is callable by **any** signed account and covers **any**
    /// non-terminal state (Pending/Accepted/Running/AwaitingVerification), so it
    /// also reclaims provider-abandoned Accepted/Running orphans a requestor-side
    /// cancel can never touch. Sets `Failed`, refunds escrow to the requestor, and
    /// frees a global `ActiveJobCount` slot.
    pub async fn expire_job(&self, job_id: u64) -> Result<(), Box<dyn std::error::Error>> {
        self.submit_signed_call(
            "ComputeMarketplace",
            "expire_job",
            vec![Value::u128(job_id as u128)],
        )
        .await
    }

    /// Inclusion-free `expire_job` for the shared nonce stream.
    pub async fn expire_job_fast(&self, job_id: u64) -> Result<(), Box<dyn std::error::Error>> {
        self.submit_fast(
            "ComputeMarketplace",
            "expire_job",
            vec![Value::u128(job_id as u128)],
        )
        .await
    }

    /// Read a job's on-chain expiry timing: `(created_at, max_duration)` in blocks.
    /// The reclaim deadline is `created_at + max_duration`. `None` if the job
    /// doesn't exist / the chain is unreachable.
    pub async fn get_job_timing(&self, job_id: u64) -> Option<(u32, u32)> {
        let client = self.client.as_ref()?;
        let addr = subxt::dynamic::storage(
            "ComputeMarketplace",
            "Jobs",
            vec![Value::u128(job_id as u128)],
        );
        let v = self.storage_at_head(client, &addr).await?;
        Some((
            get_u32_field(&v, "created_at")?,
            get_u32_field(&v, "max_duration")?,
        ))
    }

    /// Read the on-chain job record: (state, price, requestor, provider).
    /// Returns None if the job doesn't exist / chain unreachable.
    pub async fn get_job_state(
        &self,
        job_id: u64,
    ) -> Option<(String, u128, Option<String>, Option<String>)> {
        let client = self.client.as_ref()?;
        let addr = subxt::dynamic::storage(
            "ComputeMarketplace",
            "Jobs",
            vec![Value::u128(job_id as u128)],
        );
        let v = self.storage_at_head(client, &addr).await?;
        let state = get_field(&v, "state")
            .and_then(|t| {
                use subxt::ext::scale_value::ValueDef;
                match &t.value {
                    ValueDef::Variant(v) => Some(v.name.clone()),
                    _ => t.as_u128().map(|n| format!("State#{}", n)),
                }
            })
            .unwrap_or_else(|| "?".to_string());
        let price = get_u128_field(&v, "price").unwrap_or(0);

        let req_addr = subxt::dynamic::storage(
            "ComputeMarketplace",
            "JobRequestor",
            vec![Value::u128(job_id as u128)],
        );
        let prov_addr = subxt::dynamic::storage(
            "ComputeMarketplace",
            "JobProvider",
            vec![Value::u128(job_id as u128)],
        );
        let requestor = self
            .storage_at_head(client, &req_addr)
            .await
            .map(|x| account_id_to_string(&x));
        let provider = self
            .storage_at_head(client, &prov_addr)
            .await
            .map(|x| account_id_to_string(&x));
        Some((state, price, requestor, provider))
    }

    /// Read whether the account is a registered provider.
    pub async fn is_provider_registered(&self, address: &str) -> bool {
        let Some(client) = &self.client else {
            return false;
        };
        let Ok(addr32) = address.parse::<subxt::utils::AccountId32>() else {
            return false;
        };
        let key = (addr32.as_ref() as &[u8; 32]).to_vec();
        let addr = subxt::dynamic::storage(
            "ComputeMarketplace",
            "RegisteredProviders",
            vec![Value::from_bytes(key)],
        );
        self.storage_at_head(client, &addr).await.is_some()
    }

    /// Enumerate every provider's **on-chain advertised endpoint** from
    /// `ComputeMarketplace::ProviderEndpoints`, as `(hex account, url)`.
    ///
    /// Together with `RegisteredProviders` (the pallet gates `set_provider_endpoint`
    /// behind registration, so every endpoint's account IS a registered provider),
    /// this is what lets an access point / scheduler DISCOVER providers from the
    /// chain instead of a hand-configured `--providers` list (item 1: on-chain
    /// provider discovery). The URL is the provider's reachable `/status`
    /// heartbeat. Returns an empty vec if the chain is unreachable or no provider
    /// has advertised an endpoint yet.
    pub async fn list_provider_endpoints(&self) -> Vec<(String, String)> {
        let Some(client) = self.client.as_ref() else {
            return Vec::new();
        };
        let addr = subxt::dynamic::storage("ComputeMarketplace", "ProviderEndpoints", vec![]);
        let mut out: Vec<(String, String)> = Vec::new();
        if let Some(hash) = self.head_block_hash().await {
            if let Ok(mut stream) = client.storage().at(hash).iter(addr).await {
                while let Some(Ok(kv)) = stream.next().await {
                    let who = kv
                        .keys
                        .first()
                        .map(account_id_to_string)
                        .unwrap_or_default();
                    if let Ok(v) = kv.value.to_value() {
                        let bytes = bytes_from_dynamic(&v.remove_context(), 256);
                        let url = String::from_utf8_lossy(&bytes).trim().to_string();
                        if !url.is_empty() {
                            out.push((who, url));
                        }
                    }
                }
            }
        }
        out.sort();
        out
    }

    /// Read the next job id (so the demo can predict the id before accepting).
    pub async fn get_next_job_id(&self) -> u64 {
        let Some(client) = &self.client else { return 0 };
        let addr = subxt::dynamic::storage("ComputeMarketplace", "NextJobId", vec![]);
        match self.storage_at_head(client, &addr).await {
            Some(v) => v.as_u128().map(|x| x as u64).unwrap_or(0),
            None => 0,
        }
    }

    /// Read a job's minimum GPU-throughput requirement (`GPURequirements::
    /// min_gflops`). 0 = no throughput gate. Returns None if the job doesn't
    /// exist / chain unreachable.
    pub async fn get_job_min_gflops(&self, job_id: u64) -> Option<u64> {
        let client = self.client.as_ref()?;
        let addr = subxt::dynamic::storage(
            "ComputeMarketplace",
            "Jobs",
            vec![Value::u128(job_id as u128)],
        );
        let v = self.storage_at_head(client, &addr).await?;
        // Jobs.gpu_requirements.min_gflops — nested named composite.
        let req = get_field(&v, "gpu_requirements")?;
        get_u64_field(req, "min_gflops")
    }

    /// Enumerate marketplace jobs that are currently `Pending` and that this
    /// provider can actually satisfy (attested GFLOPS >= job.min_gflops). This
    /// is the loop-closing primitive: `auto` mode scans for claimable work
    /// when the oracle says compute demand is high. Returns (job_id, min_gflops).
    ///
    /// `attested_gflops` = the provider's on-chain attested compute throughput;
    /// a job with min_gflops=0 is always claimable (open market).
    pub async fn list_claimable_jobs(&self, attested_gflops: u64) -> Vec<(u64, u64)> {
        let Some(client) = &self.client else {
            return vec![];
        };
        let next = self.get_next_job_id().await;
        let mut out = Vec::new();
        for id in 0..next {
            let addr = subxt::dynamic::storage(
                "ComputeMarketplace",
                "Jobs",
                vec![Value::u128(id as u128)],
            );
            let Some(v) = self.storage_at_head(client, &addr).await else {
                continue;
            };
            // Only Pending jobs are claimable (skip Accepted/Running/etc).
            let state = get_field(&v, "state")
                .and_then(|t| {
                    use subxt::ext::scale_value::ValueDef;
                    match &t.value {
                        ValueDef::Variant(v) => Some(v.name.clone()),
                        _ => None,
                    }
                })
                .unwrap_or_default();
            if state != "Pending" {
                continue;
            }
            let min_gflops = get_field(&v, "gpu_requirements")
                .and_then(|r| get_u64_field(r, "min_gflops"))
                .unwrap_or(0);
            if is_job_claimable(min_gflops, attested_gflops) {
                out.push((id, min_gflops));
            }
        }
        out
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

    // ---- Liveness (full-node / bootnode self-organization) --------------

    /// Register this node as an always-on full node with a public endpoint.
    /// Signed `Liveness::register_fullnode`. Deposits the liveness bond from
    /// the signer's free balance. Idempotent-ish: the pallet rejects a
    /// duplicate registration with `AlreadyRegistered`.
    pub async fn register_fullnode(
        &self,
        endpoint: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(uri) = &self.signer_uri else {
            log::warn!("No signer configured; cannot register full node (set signer_uri).");
            return Ok(());
        };
        let signer = signer::Sr25519Signer::from_uri(uri)?;
        let client = signer::connect(&self.endpoint).await?;
        let endpoint_bytes = endpoint.as_bytes().to_vec();
        let call = subxt::dynamic::tx(
            "Liveness",
            "register_fullnode",
            vec![Value::from_bytes(endpoint_bytes)],
        );
        let nonce = self
            .get_account_nonce(&signer.account_id().to_string())
            .await;
        signer::submit_signed(&client, &call, &signer, nonce).await?;
        log::info!(
            "Registered as a full node on-chain (endpoint {}).",
            endpoint
        );
        Ok(())
    }

    /// A bonded full node witnesses that it successfully reached `target`'s
    /// declared endpoint this window. Signed `Liveness::witness_reachable`.
    pub async fn witness_reachable(&self, target: &str) -> Result<(), Box<dyn std::error::Error>> {
        let Some(uri) = &self.signer_uri else {
            log::warn!("No signer configured; cannot witness (set signer_uri).");
            return Ok(());
        };
        let signer = signer::Sr25519Signer::from_uri(uri)?;
        let client = signer::connect(&self.endpoint).await?;
        let target32: subxt::utils::AccountId32 = target.parse()?;
        let key = (target32.as_ref() as &[u8; 32]).to_vec();
        let call = subxt::dynamic::tx(
            "Liveness",
            "witness_reachable",
            vec![Value::from_bytes(key)],
        );
        let nonce = self
            .get_account_nonce(&signer.account_id().to_string())
            .await;
        signer::submit_signed(&client, &call, &signer, nonce).await?;
        log::info!("Witnessed full node {} reachable this window.", target);
        Ok(())
    }

    /// Finalize the most recent completed liveness window for `target` (anyone
    /// may call). Signed `Liveness::submit_liveness`. This is how a node
    /// confirms it stayed online (or records a miss).
    pub async fn submit_liveness(&self, target: &str) -> Result<(), Box<dyn std::error::Error>> {
        let Some(uri) = &self.signer_uri else {
            log::warn!("No signer configured; cannot finalize liveness (set signer_uri).");
            return Ok(());
        };
        let signer = signer::Sr25519Signer::from_uri(uri)?;
        let client = signer::connect(&self.endpoint).await?;
        let target32: subxt::utils::AccountId32 = target.parse()?;
        let key = (target32.as_ref() as &[u8; 32]).to_vec();
        let call = subxt::dynamic::tx("Liveness", "submit_liveness", vec![Value::from_bytes(key)]);
        let nonce = self
            .get_account_nonce(&signer.account_id().to_string())
            .await;
        signer::submit_signed(&client, &call, &signer, nonce).await?;
        log::info!("Finalized liveness window for {}.", target);
        Ok(())
    }

    /// Withdraw the liveness bond and stop being a full node. Signed
    /// `Liveness::unregister_fullnode`.
    pub async fn unregister_fullnode(&self) -> Result<(), Box<dyn std::error::Error>> {
        let Some(uri) = &self.signer_uri else {
            log::warn!("No signer configured; cannot unregister (set signer_uri).");
            return Ok(());
        };
        let signer = signer::Sr25519Signer::from_uri(uri)?;
        let client = signer::connect(&self.endpoint).await?;
        let call = subxt::dynamic::tx("Liveness", "unregister_fullnode", Vec::<Value>::new());
        let nonce = self
            .get_account_nonce(&signer.account_id().to_string())
            .await;
        signer::submit_signed(&client, &call, &signer, nonce).await?;
        log::info!("Unregistered full node; bond returned.");
        Ok(())
    }

    /// Read this node's liveness record: (active, missed_count, last_window,
    /// uptime_bps). None if not registered. Reads via dynamic storage.
    pub async fn get_fullnode_status(&self, address: &str) -> Option<(bool, u32, u64, u32)> {
        let client = self.client.as_ref()?;
        let addr32 = address.parse::<subxt::utils::AccountId32>().ok()?;
        let key = (addr32.as_ref() as &[u8; 32]).to_vec();
        let rec = subxt::dynamic::storage("Liveness", "FullNodes", vec![Value::from_bytes(key)]);
        let v = self.storage_at_head(client, &rec).await?;
        let active = get_field(&v, "active")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        let missed = get_u32_field(&v, "missed_count").unwrap_or(0);
        let last_win = get_u64_field(&v, "last_confirmed_window").unwrap_or(0);
        // Compute uptime_bps from the bitmask via popcount.
        let bits = get_u32_field(&v, "uptime_bits").unwrap_or(0);
        let uptime = popcount(bits) * 10_000u32 / 10u32;
        Some((active, missed, last_win, uptime))
    }

    /// `(LivenessWindow, UptimeLookback)` from the live runtime metadata.
    /// Falls back to the runtime defaults (14_400 blocks, 10 windows) when the
    /// constants are unreadable, so the exporter never invents an uptime.
    pub async fn liveness_constants(&self) -> (u64, u32) {
        let Some(client) = self.client.as_ref() else {
            return (14_400, 10);
        };
        let window = client
            .constants()
            .at(&subxt::dynamic::constant("Liveness", "LivenessWindow"))
            .ok()
            .and_then(|c| c.to_value().ok())
            .and_then(|v| v.as_u128())
            .map(|v| v as u64)
            .filter(|v| *v > 0)
            .unwrap_or(14_400);
        let lookback = client
            .constants()
            .at(&subxt::dynamic::constant("Liveness", "UptimeLookback"))
            .ok()
            .and_then(|c| c.to_value().ok())
            .and_then(|v| v.as_u128())
            .map(|v| v as u32)
            .filter(|v| *v > 0)
            .unwrap_or(10);
        (window, lookback)
    }

    /// Every registered full node with its liveness record, declared endpoint
    /// and current-window witness count — the input to the seed-list exporter
    /// (`gpn_agent::seedlist`, design §3.4).
    ///
    /// `reachable` is the *maximum* of the current and previous window's distinct
    /// witness counts: witnesses are submitted during a window and finalised a
    /// window later, so reading only the current one would publish an empty list
    /// early in every window.
    pub async fn list_liveness_nodes(&self) -> Vec<crate::seedlist::NodeLiveness> {
        use std::collections::HashMap;
        let Some(client) = self.client.as_ref() else {
            return Vec::new();
        };
        let Some(hash) = self.head_block_hash().await else {
            return Vec::new();
        };

        // Declared endpoints: hex account -> multiaddr string.
        let mut endpoints: HashMap<String, String> = HashMap::new();
        let ep_addr = subxt::dynamic::storage("Liveness", "Endpoints", vec![]);
        if let Ok(mut stream) = client.storage().at(hash).iter(ep_addr).await {
            while let Some(Ok(kv)) = stream.next().await {
                let Some(key) = kv.keys.first() else { continue };
                let account = account_id_to_string(key);
                if let Ok(v) = kv.value.to_value() {
                    let bytes = bytes_from_dynamic(&v.remove_context(), 512);
                    let s = String::from_utf8_lossy(&bytes).trim().to_string();
                    if !s.is_empty() {
                        endpoints.insert(account, s);
                    }
                }
            }
        }

        let (window_blocks, lookback) = self.liveness_constants().await;
        let head = self.head_block_number().await.unwrap_or(0);
        let current_window = if window_blocks == 0 { 0 } else { head / window_blocks };

        let mut out = Vec::new();
        let fn_addr = subxt::dynamic::storage("Liveness", "FullNodes", vec![]);
        let Ok(mut stream) = client.storage().at(hash).iter(fn_addr).await else {
            return out;
        };
        while let Some(Ok(kv)) = stream.next().await {
            let Some(key) = kv.keys.first() else { continue };
            let account = account_id_to_string(key);
            let mut account_bytes: Vec<u8> = Vec::with_capacity(32);
            collect_account_bytes(key, &mut account_bytes);
            if account_bytes.len() != 32 {
                continue;
            }
            let Ok(mut value) = kv.value.to_value() else {
                continue;
            };
            let v = value.remove_context();
            let active = get_field(&v, "active")
                .and_then(|x| x.as_bool())
                .unwrap_or(false);
            let bits = get_u32_field(&v, "uptime_bits").unwrap_or(0);
            let last_window = get_u64_field(&v, "last_confirmed_window").unwrap_or(0);
            let uptime_bps = popcount(bits) * 10_000u32 / lookback;

            let mut reachable = 0u32;
            for win in [current_window, current_window.saturating_sub(1)] {
                let key = vec![
                    Value::from(win),
                    Value::from_bytes(account_bytes.as_slice()),
                ];
                let wc = subxt::dynamic::storage("Liveness", "WitnessCounts", key);
                if let Some(val) = self.storage_at_head(client, &wc).await {
                    reachable = reachable.max(val.as_u128().unwrap_or(0) as u32);
                }
            }

            out.push(crate::seedlist::NodeLiveness {
                account: account.clone(),
                endpoint: endpoints.get(&account).cloned().unwrap_or_default(),
                active,
                uptime_bps,
                uptime_bits: bits,
                last_confirmed_window: last_window,
                reachable,
            });
        }
        out.sort_by(|a, b| a.account.cmp(&b.account));
        out
    }

    /// Current liveness window id (`head / LivenessWindow`), for exporters.
    pub async fn current_liveness_window(&self) -> u64 {
        let (window_blocks, _) = self.liveness_constants().await;
        if window_blocks == 0 {
            return 0;
        }
        self.head_block_number().await.unwrap_or(0) / window_blocks
    }

    // ---- Treasury (vested, governance-controlled) -----------------------

    /// The treasury pallet account (mirrors `pallet_gpn_treasury::account_id`).
    pub fn treasury_account(&self) -> String {
        let mut bytes = [0u8; 32];
        bytes[0..4].copy_from_slice(b"modl");
        bytes[4..12].copy_from_slice(b"gpn/trsy");
        subxt::utils::AccountId32::from(bytes).to_string()
    }

    /// Treasury pallet account free balance (base units, 12 decimals).
    pub async fn treasury_balance(&self) -> Option<u128> {
        let client = self.client.as_ref()?;
        let acct = self.treasury_account();
        let addr32 = acct.parse::<subxt::utils::AccountId32>().ok()?;
        let storage_addr = subxt::dynamic::storage(
            "System",
            "Account",
            vec![Value::from_bytes(addr32.as_ref() as &[u8; 32])],
        );
        let v = self.storage_at_head(client, &storage_addr).await?;
        get_field(&v, "data").and_then(|d| get_u128_field(d, "free"))
    }

    /// The governance approval threshold (e.g. 13).
    pub async fn treasury_threshold(&self) -> Option<u32> {
        let client = self.client.as_ref()?;
        let addr = subxt::dynamic::storage("GPNTreasury", "Threshold", Vec::<Value>::new());
        let v = self.storage_at_head(client, &addr).await?;
        Some(v.as_u128()? as u32)
    }

    /// Total GPN already released from the treasury (base units).
    pub async fn treasury_total_released(&self) -> Option<u128> {
        let client = self.client.as_ref()?;
        let addr = subxt::dynamic::storage("GPNTreasury", "TotalReleased", Vec::<Value>::new());
        let v = self.storage_at_head(client, &addr).await?;
        v.as_u128()
    }

    /// Roster size (number of governance addresses).
    pub async fn treasury_roster_len(&self) -> Option<usize> {
        let client = self.client.as_ref()?;
        let addr = subxt::dynamic::storage("GPNTreasury", "Roster", Vec::<Value>::new());
        let v = self.storage_at_head(client, &addr).await?;
        Some(bounded_vec_len(&v))
    }

    /// The vesting schedule as `(unlock_block, amount_base)` pairs.
    pub async fn treasury_schedule(&self) -> Option<Vec<(u64, u128)>> {
        let client = self.client.as_ref()?;
        let addr = subxt::dynamic::storage("GPNTreasury", "Schedule", Vec::<Value>::new());
        let v = self.storage_at_head(client, &addr).await?;
        Some(parse_tranche_seq(&v))
    }

    /// Current head block number (public wrapper) for vesting math in the CLI.
    pub async fn head_number(&self) -> Option<u64> {
        self.head_block_number().await
    }
}

/// Whether a profitability feed written at block `updated_at_block` is stale
/// given the current head block and a TTL of `ttl_blocks`. A feed is stale once
/// the chain has advanced more than `ttl_blocks` past its `updated_at`.
///
/// A TTL of `None` (disabled) or `Some(0)` never considers the feed stale —
/// used by tests / operators who explicitly disable the guard.
///
/// Pure free function so it is unit-testable without a live chain.
pub fn is_feed_stale(updated_at_block: u64, head_block: u64, ttl_blocks: Option<u64>) -> bool {
    match ttl_blocks {
        None | Some(0) => false,
        Some(ttl) => head_block.saturating_sub(updated_at_block) > ttl,
    }
}

/// Median of a set of finite f64s (oracle-governance §6 gap #3). NaN/Inf
/// values are ignored. Returns `None` if the (finite) set is empty. The
/// median is robust to outliers: one extreme operator value cannot move it.
pub fn median_f64(values: &[f64]) -> Option<f64> {
    let mut v: Vec<f64> = values.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
    let mid = v.len() / 2;
    if v.len() % 2 == 1 {
        Some(v[mid])
    } else {
        Some((v[mid - 1] + v[mid]) / 2.0)
    }
}

/// Aggregate several operators' latest feeds into a single feed by taking the
/// **median** of each numeric field (token price, GPU-hour price, utilization,
/// staking APR) and the **max** `updated_at_block` (the most recent write).
/// One outlier operator therefore cannot move any field — this is the
/// oracle-governance §6 gap #3 client-side aggregation rule. Returns `None`
/// if `feeds` is empty.
pub fn aggregate_median_feed(feeds: &[OnChainFeed]) -> Option<OnChainFeed> {
    if feeds.is_empty() {
        return None;
    }
    Some(OnChainFeed {
        token_price_usd: median_f64(&feeds.iter().map(|f| f.token_price_usd).collect::<Vec<_>>())?,
        avg_gpu_hour_price_usd: median_f64(
            &feeds
                .iter()
                .map(|f| f.avg_gpu_hour_price_usd)
                .collect::<Vec<_>>(),
        )?,
        utilization: median_f64(&feeds.iter().map(|f| f.utilization).collect::<Vec<_>>())?,
        staking_apr_percent: median_f64(
            &feeds
                .iter()
                .map(|f| f.staking_apr_percent)
                .collect::<Vec<_>>(),
        )?,
        updated_at_block: feeds.iter().map(|f| f.updated_at_block).max().unwrap_or(0),
    })
}

/// Bond-weighted median of a field across operators (Phase-C oracle
/// aggregation, spec §5/§8 D3). `weights` and `values` are parallel (index i
/// is one operator); operators with zero/absent weight are excluded. Sorts by
/// field value, walks cumulative bond weight, and returns the value at the
/// 50% cumulative-weight point — so a large-bond operator dominates the
/// median and a tiny-bond outlier cannot move it. Returns `None` if no
/// positive-weight finite values remain.
pub fn bond_weighted_median_f64(weights: &[f64], values: &[f64]) -> Option<f64> {
    let mut pairs: Vec<(f64, f64)> = values
        .iter()
        .copied()
        .zip(weights.iter().copied())
        .filter(|(v, w)| v.is_finite() && *w > 0.0)
        .collect();
    if pairs.is_empty() {
        return None;
    }
    pairs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(core::cmp::Ordering::Equal));
    let total: f64 = pairs.iter().map(|(_, w)| w).sum();
    if total <= 0.0 {
        return None;
    }
    let half = total / 2.0;
    let mut cum = 0.0;
    for (v, w) in &pairs {
        cum += w;
        if cum >= half {
            return Some(*v);
        }
    }
    pairs.last().map(|(v, _)| *v)
}

/// Aggregate several operators' latest feeds into a single **bond-weighted
/// median** feed (Phase-C oracle aggregation, spec §5/§8 D3). `feeds` and
/// `bonds` are parallel (index i is one operator); an operator's bond is its
/// weight, so a large-bond operator dominates and a tiny-bond outlier cannot
/// move any field. `updated_at_block` is the max (most recent write). Returns
/// `None` if `feeds` is empty or no positive-weight finite values remain.
pub fn aggregate_bonded_median_feed(feeds: &[OnChainFeed], bonds: &[u128]) -> Option<OnChainFeed> {
    if feeds.is_empty() || feeds.len() != bonds.len() {
        return None;
    }
    let w: Vec<f64> = bonds.iter().map(|b| *b as f64).collect();
    Some(OnChainFeed {
        token_price_usd: bond_weighted_median_f64(
            &w,
            &feeds.iter().map(|f| f.token_price_usd).collect::<Vec<_>>(),
        )?,
        avg_gpu_hour_price_usd: bond_weighted_median_f64(
            &w,
            &feeds
                .iter()
                .map(|f| f.avg_gpu_hour_price_usd)
                .collect::<Vec<_>>(),
        )?,
        utilization: bond_weighted_median_f64(
            &w,
            &feeds.iter().map(|f| f.utilization).collect::<Vec<_>>(),
        )?,
        staking_apr_percent: bond_weighted_median_f64(
            &w,
            &feeds
                .iter()
                .map(|f| f.staking_apr_percent)
                .collect::<Vec<_>>(),
        )?,
        updated_at_block: feeds.iter().map(|f| f.updated_at_block).max().unwrap_or(0),
    })
}

/// Parse a `ProfitabilityFeed` scale composite (already fetched as a dynamic
/// `Value`) into the agent's dollar / fraction / percent `OnChainFeed` form.
/// Returns `None` if any required field is missing/malformed. Shared by the
/// `CurrentFeed` reader and the per-operator `OperatorFeeds` reader.
fn parse_feed_value(fields: &subxt::dynamic::Value) -> Option<OnChainFeed> {
    let token_micro = get_u64_field(fields, "token_price_usd")?;
    let gpu_micro = get_u64_field(fields, "avg_gpu_hour_price")?;
    let utilization_bps = get_u32_field(fields, "utilization_rate_bps")? as u64;
    let apr_bps = get_u32_field(fields, "staking_apr_bps")? as u64;
    let updated_at = get_u64_field(fields, "updated_at")?;
    Some(OnChainFeed {
        token_price_usd: token_micro as f64 / 1_000_000.0,
        avg_gpu_hour_price_usd: gpu_micro as f64 / 1_000_000.0,
        utilization: utilization_bps as f64 / 10_000.0,
        staking_apr_percent: apr_bps as f64 / 100.0,
        updated_at_block: updated_at,
    })
}

/// Whether a provider with `attested_gflops` GFLOPS can claim a job that
/// requires `min_gflops`. A 0 threshold (open market) is always claimable;
/// otherwise the provider must meet or exceed the requirement. Pure free
/// function so it is unit-testable without a live chain.
fn is_job_claimable(min_gflops: u64, attested_gflops: u64) -> bool {
    min_gflops == 0 || attested_gflops >= min_gflops
}

/// Convert a little-endian `[u64;4]` target into a classic difficulty metric
/// `2^256 / target`. Target is never zero in practice (pallet clamps it), but
/// we guard anyway to avoid dividing by zero.
fn target_to_difficulty(limbs: [u64; 4]) -> f64 {
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

/// Collect up to `cap` bytes from a dynamically-decoded `Vec<u8>` (which subxt
/// decodes as an unnamed composite of u128 byte primitives). Unlike
/// `composite_to_bytes` this is variable-length (not fixed at 32), so it can
/// read an on-chain `Vec<u8>` such as a provider endpoint URL.
fn bytes_from_dynamic(value: &subxt::dynamic::Value, cap: usize) -> Vec<u8> {
    use subxt::ext::scale_value::ValueDef;
    let mut out = Vec::new();
    if let ValueDef::Composite(c) = &value.value {
        for v in c.values() {
            if out.len() >= cap {
                break;
            }
            // A `u8` may decode as a u128 primitive or as a nested composite.
            match &v.value {
                ValueDef::Primitive(subxt::ext::scale_value::Primitive::U128(n)) => {
                    out.push((*n & 0xff) as u8)
                }
                ValueDef::Composite(_) => out.extend(bytes_from_dynamic(v, cap - out.len())),
                _ => {}
            }
        }
    }
    out
}

/// Look up a named u64 field inside a scale composite value.
fn get_u64_field(value: &subxt::dynamic::Value, name: &str) -> Option<u64> {
    get_field(value, name)
        .and_then(|v| v.as_u128())
        .map(|v| v as u64)
}

/// Population count of a u32 (number of set bits) — used to compute rolling
/// uptime from the liveness pallet's `uptime_bits` window bitmask.
fn popcount(mut x: u32) -> u32 {
    let mut c = 0u32;
    while x != 0 {
        c += x & 1;
        x >>= 1;
    }
    c
}

/// Elements of a `BoundedVec` decoded by subxt as a scale-value.
///
/// subxt represents `BoundedVec<T, N>` as a 1-field composite wrapping the
/// inner `Vec` (itself a composite of elements): `Composite([Composite([...])])`.
/// This unwraps that wrapper and returns the element list (works for empty).
fn bounded_vec_elems(v: &subxt::dynamic::Value) -> Vec<&subxt::dynamic::Value> {
    use subxt::ext::scale_value::ValueDef;
    match &v.value {
        ValueDef::Composite(c) => {
            let vals: Vec<&subxt::dynamic::Value> = c.values().collect();
            if vals.len() == 1 {
                if let ValueDef::Composite(inner) = &vals[0].value {
                    return inner.values().collect();
                }
            }
            vals
        }
        _ => Vec::new(),
    }
}

/// Number of elements in a `BoundedVec` decoded as a scale-value composite.
fn bounded_vec_len(v: &subxt::dynamic::Value) -> usize {
    bounded_vec_elems(v).len()
}

/// Parse a `BoundedVec<(BlockNumber, Balance)>` (vesting schedule) from a
/// decoded scale-value. Each element is an unnamed 2-tuple `(block, amount)`.
fn parse_tranche_seq(v: &subxt::dynamic::Value) -> Vec<(u64, u128)> {
    use subxt::ext::scale_value::ValueDef;
    let mut out = Vec::new();
    for elem in bounded_vec_elems(v) {
        if let ValueDef::Composite(t) = &elem.value {
            let mut it = t.values();
            let blk = it.next().and_then(|x| x.as_u128()).unwrap_or(0) as u64;
            let amt = it.next().and_then(|x| x.as_u128()).unwrap_or(0);
            out.push((blk, amt));
        }
    }
    out
}

fn get_u32_field(value: &subxt::dynamic::Value, name: &str) -> Option<u32> {
    get_u64_field(value, name).map(|v| v as u32)
}

fn get_u8_field(value: &subxt::dynamic::Value, name: &str) -> Option<u8> {
    get_u64_field(value, name).map(|v| v as u8)
}

fn get_u128_field(value: &subxt::dynamic::Value, name: &str) -> Option<u128> {
    get_field(value, name).and_then(|v| v.as_u128())
}

/// Look up a named field inside a scale composite value (returns the sub-value).
fn get_field<'a>(
    value: &'a subxt::dynamic::Value,
    name: &str,
) -> Option<&'a subxt::dynamic::Value> {
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

/// Convert a scale-value AccountId to a hex string for display.
/// Used when reading `JobRequestor`/`JobProvider`.
///
/// In subxt dynamic storage reads an `AccountId32` decodes as a **doubly-
/// nested** `Composite::Unnamed`: the outer composite holds one inner
/// composite of 32 byte-primitives. We therefore recursively descend into any
/// nested composites and collect byte-sized (0..=255) leaf primitives, up to
/// 32 bytes — robust to the actual shape subxt produces. The old code only
/// read the outer level's values, so a `[u8; 32]` account silently rendered as
/// all-zeros (`provider=0x00`) in cycle 28.
fn account_id_to_string(value: &subxt::dynamic::Value) -> String {
    let mut bytes = Vec::with_capacity(32);
    collect_account_bytes(value, &mut bytes);
    if bytes.is_empty() {
        return format!("{:?}", value.value);
    }
    format!("0x{}", hex::encode(&bytes))
}

/// Recursively collect up to 32 byte-sized (0..=255) leaf primitives from a
/// scale-value tree, descending through any nested composites/variants.
fn collect_account_bytes(value: &subxt::dynamic::Value, out: &mut Vec<u8>) {
    use subxt::ext::scale_value::ValueDef;
    if out.len() >= 32 {
        return;
    }
    match &value.value {
        ValueDef::Composite(c) => {
            for v in c.values() {
                collect_account_bytes(v, out);
            }
        }
        ValueDef::Variant(v) => {
            for v in v.values.values() {
                collect_account_bytes(v, out);
            }
        }
        ValueDef::Primitive(subxt::ext::scale_value::Primitive::U128(n)) => {
            if *n <= 255 {
                out.push(*n as u8);
            }
        }
        ValueDef::Primitive(subxt::ext::scale_value::Primitive::U256(b)) => {
            for byte in b.iter().take(32 - out.len()) {
                out.push(*byte);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_to_difficulty_matches_classic_formula() {
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
    fn is_job_claimable_open_market_always_claimable() {
        // min_gflops == 0 (open market): claimable regardless of attested power
        assert!(is_job_claimable(0, 0));
        assert!(is_job_claimable(0, 188));
    }

    #[test]
    fn is_job_claimable_gated_by_attested_gflops() {
        // provider attested 188 GFLOPS (cycle-28 4060)
        assert!(is_job_claimable(150, 188)); // meets requirement
        assert!(is_job_claimable(188, 188)); // exact match ok
        assert!(!is_job_claimable(1000, 188)); // below requirement -> NOT claimable
    }

    #[test]
    fn is_job_claimable_unattested_provider() {
        // 0 attested GFLOPS (never attested) can only take open-market jobs
        assert!(is_job_claimable(0, 0));
        assert!(!is_job_claimable(150, 0));
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
    fn bytes_from_dynamic_reads_variable_length_vec_u8() {
        // An on-chain `Vec<u8>` (e.g. a provider endpoint URL) decodes as an
        // unnamed composite of u128 byte primitives; it must round-trip fully,
        // not be truncated to the fixed 32-byte account width.
        let url = b"http://gpn-testnet.duckdns.org:8093/status";
        let v = subxt::dynamic::Value::from_bytes(url);
        let out = bytes_from_dynamic(&v, 256);
        assert_eq!(&out[..], &url[..]);
        assert_eq!(
            String::from_utf8_lossy(&out),
            "http://gpn-testnet.duckdns.org:8093/status"
        );
    }

    #[test]
    fn bond_weighted_median_large_bond_dominates_tiny_bond_outlier() {
        // Three operators: a huge-bond outlier and a huge-bond inlier.
        // Weighted median must follow the large bond, not the small bond.
        // Values: op A bond 1000 feed 2.0, op B bond 1 feed 9999.0,
        // op C bond 1000 feed 3.0. Sorted by value: 2.0(w1000), 3.0(w1000), 9999.0(w1).
        // Cumulative: 1000 (50% of 2001 ≈ 1000.5) → 2.0, then 2000 ≥ 1000.5 → 3.0.
        let w = vec![1000.0, 1.0, 1000.0];
        let v = vec![2.0, 9_999.0, 3.0];
        let m = bond_weighted_median_f64(&w, &v).expect("weighted median");
        // The tiny-bond outlier ($9,999) must NOT be the answer; the weighted
        // median lands on 3.0 (the second large-bond operator), exactly the
        // plain median [2,3,9999]→3 too — but here weight pushes it toward the
        // dominant operators even when their count is equal.
        assert!((m - 3.0).abs() < 1e-9, "got {m}");
    }

    #[test]
    fn bond_weighted_median_single_operator() {
        let m = bond_weighted_median_f64(&[500.0], &[7.5]).expect("weighted median");
        assert!((m - 7.5).abs() < 1e-9);
    }

    #[test]
    fn bond_weighted_median_ignores_zero_bond_and_non_finite() {
        // Zero/absent bond excluded; NaN value excluded.
        let w = vec![0.0, 100.0, 50.0];
        let v = vec![f64::NAN, 4.0, 2.0];
        // Only (4.0,w100) and (2.0,w50) remain: sorted 2.0(w50),4.0(w100);
        // half of 150 = 75 → cum 50 < 75, cum 150 ≥ 75 → 4.0.
        let m = bond_weighted_median_f64(&w, &v).expect("weighted median");
        assert!((m - 4.0).abs() < 1e-9, "got {m}");
    }

    #[test]
    fn bond_weighted_median_empty_or_all_zero_weights_is_none() {
        assert!(bond_weighted_median_f64(&[], &[]).is_none());
        assert!(bond_weighted_median_f64(&[0.0, 0.0], &[1.0, 2.0]).is_none());
    }

    #[test]
    fn aggregate_bonded_median_feed_weights_by_bond() {
        // Two large-bond operators + one tiny-bond outlier.
        let a = OnChainFeed {
            token_price_usd: 0.05,
            avg_gpu_hour_price_usd: 2.0,
            utilization: 0.60,
            staking_apr_percent: 12.0,
            updated_at_block: 10,
        };
        let b = OnChainFeed {
            token_price_usd: 0.07,
            avg_gpu_hour_price_usd: 3.0,
            utilization: 0.70,
            staking_apr_percent: 15.0,
            updated_at_block: 20,
        };
        let c = OnChainFeed {
            token_price_usd: 0.06,
            avg_gpu_hour_price_usd: 9_999.0,
            utilization: 0.80,
            staking_apr_percent: 18.0,
            updated_at_block: 30,
        };
        // Bonds: a=1000, b=1000, c=1 (tiny-bond outlier on gpu price).
        let bonds = vec![1000u128, 1000, 1];
        let m = aggregate_bonded_median_feed(&[a, b, c], &bonds).expect("bonded median feed");
        // The tiny-bond $9,999/hr outlier must NOT move gpu price (weighted
        // median lands on 3.0, same as the plain median here).
        assert!(
            (m.avg_gpu_hour_price_usd - 3.0).abs() < 1e-9,
            "got {}",
            m.avg_gpu_hour_price_usd
        );
        assert!((m.token_price_usd - 0.06).abs() < 1e-9);
        assert!((m.utilization - 0.70).abs() < 1e-9);
        assert!((m.staking_apr_percent - 15.0).abs() < 1e-9);
        assert_eq!(m.updated_at_block, 30); // max
    }

    #[test]
    fn aggregate_bonded_median_feed_len_mismatch_or_empty_is_none() {
        let a = OnChainFeed {
            token_price_usd: 0.05,
            avg_gpu_hour_price_usd: 2.0,
            utilization: 0.60,
            staking_apr_percent: 12.0,
            updated_at_block: 10,
        };
        assert!(aggregate_bonded_median_feed(&[], &[]).is_none());
        assert!(aggregate_bonded_median_feed(&[a], &[]).is_none()); // len mismatch
    }

    #[test]
    fn account_id_to_string_handles_composite_and_u256_decode() {
        // An AccountId32 in a dynamic storage read decodes as either a
        // Composite::Unnamed of 32 u8s OR a Primitive::U256 (as observed live
        // in cycle 28, where the old Composite-only code rendered `provider=0x00`).
        // Verify we now render the correct hex address for both shapes.
        use subxt::ext::scale_value::Primitive;
        use subxt::ext::scale_value::Value;
        let bytes: [u8; 32] = [
            0x5a, 0x6b, 0x7c, 0x8d, 0x9e, 0xaf, 0xb0, 0xc1, 0xd2, 0xe3, 0xf4, 0x01, 0x23, 0x45,
            0x67, 0x89, 0xab, 0xcd, 0xef, 0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc, 0xfe, 0x11,
            0x22, 0x33, 0x44, 0x55,
        ];
        let expected = format!("0x{}", hex::encode(bytes));

        // The real shape subxt produces for an AccountId32: a doubly-nested
        // Composite::Unnamed — outer holds one inner composite of 32 byte-prims.
        let inner = Value::unnamed_composite(
            bytes
                .iter()
                .map(|b| Value::from(*b as u128))
                .collect::<Vec<_>>(),
        );
        let outer = Value::unnamed_composite(vec![inner]);
        assert_eq!(account_id_to_string(&outer), expected);

        // Composite::Unnamed of byte-prims directly (alternative shape)
        let flat = Value::unnamed_composite(
            bytes
                .iter()
                .map(|b| Value::from(*b as u128))
                .collect::<Vec<_>>(),
        );
        assert_eq!(account_id_to_string(&flat), expected);

        // Primitive::U256 shape (defensive)
        let u256 = Value::primitive(Primitive::U256(bytes));
        assert_eq!(account_id_to_string(&u256), expected);
    }

    #[test]
    fn account_id_to_string_does_not_render_all_zeros_for_u256() {
        // Regression guard for the cycle-28 display bug: a U256 account must
        // NOT come back as 0x00…00.
        use subxt::ext::scale_value::Primitive;
        let bytes = [0x11u8; 32];
        let u256 = Value::primitive(Primitive::U256(bytes));
        let s = account_id_to_string(&u256);
        assert_eq!(s, format!("0x{}", hex::encode([0x11u8; 32])));
        assert!(!s.contains("00"));
    }

    #[test]
    fn account_id_to_string_non_account_value_does_not_panic() {
        // Unsupported shape (e.g. a bare u128) should render something rather
        // than panic — the demo must never crash on a decode edge case.
        let v = subxt::dynamic::Value::u128(42);
        let s = account_id_to_string(&v);
        assert!(!s.is_empty());
    }

    #[test]
    fn is_feed_stale_ttl_boundary() {
        // Fresh: head within TTL of updated_at.
        assert!(!is_feed_stale(100, 100, Some(1440)));
        assert!(!is_feed_stale(100, 100 + 1440, Some(1440))); // exactly at TTL = fresh
                                                              // Stale: head has advanced more than TTL past updated_at.
        assert!(is_feed_stale(100, 100 + 1441, Some(1440)));
        assert!(is_feed_stale(100, 5000, Some(1440)));
    }

    #[test]
    fn is_feed_stale_disabled_when_ttl_none_or_zero() {
        // Explicitly disabled -> never stale, even far behind.
        assert!(!is_feed_stale(100, 500_000, None));
        assert!(!is_feed_stale(100, 500_000, Some(0)));
    }

    #[test]
    fn is_feed_stale_never_underflows() {
        // updated_at can't be in the future relative to head (saturating_sub).
        assert!(!is_feed_stale(5000, 100, Some(1440)));
        // Head == updated_at (just written) -> fresh.
        assert!(!is_feed_stale(42, 42, Some(1440)));
    }

    #[test]
    fn median_f64_odd_set() {
        assert_eq!(median_f64(&[3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median_f64(&[10.0]), Some(10.0));
    }

    #[test]
    fn median_f64_even_set() {
        // Even count -> mean of the two middle values.
        assert_eq!(median_f64(&[1.0, 2.0, 3.0, 4.0]), Some(2.5));
        assert_eq!(median_f64(&[5.0, 1.0, 3.0, 2.0, 4.0, 6.0]), Some(3.5));
    }

    #[test]
    fn median_f64_ignores_non_finite_and_empty() {
        assert_eq!(median_f64(&[]), None);
        assert_eq!(median_f64(&[f64::NAN, f64::INFINITY, 2.0]), Some(2.0));
        assert_eq!(median_f64(&[f64::NAN, f64::NEG_INFINITY]), None);
    }

    #[test]
    fn median_f64_robust_to_outlier() {
        // One extreme value must not move the median (only the mean).
        assert_eq!(median_f64(&[1.0, 2.0, 3.0, 10_000.0]), Some(2.5));
    }

    #[test]
    fn aggregate_median_feed_empty_is_none() {
        assert!(aggregate_median_feed(&[]).is_none());
    }

    #[test]
    fn aggregate_median_feed_medians_each_field_and_takes_max_updated_at() {
        let a = OnChainFeed {
            token_price_usd: 0.05,
            avg_gpu_hour_price_usd: 2.0,
            utilization: 0.60,
            staking_apr_percent: 12.0,
            updated_at_block: 10,
        };
        let b = OnChainFeed {
            token_price_usd: 0.07,
            avg_gpu_hour_price_usd: 3.0,
            utilization: 0.70,
            staking_apr_percent: 15.0,
            updated_at_block: 20,
        };
        let c = OnChainFeed {
            // Outlier GPU-hour price — must NOT move the median.
            token_price_usd: 0.06,
            avg_gpu_hour_price_usd: 9_999.0,
            utilization: 0.80,
            staking_apr_percent: 18.0,
            updated_at_block: 30,
        };
        let m = aggregate_median_feed(&[a, b, c]).expect("median feed");
        assert!((m.token_price_usd - 0.06).abs() < 1e-9);
        // Median of [2,3,9999] is 3 — the outlier is fully excluded.
        assert!((m.avg_gpu_hour_price_usd - 3.0).abs() < 1e-9);
        assert!((m.utilization - 0.70).abs() < 1e-9);
        assert!((m.staking_apr_percent - 15.0).abs() < 1e-9);
        assert_eq!(m.updated_at_block, 30); // max of {10,20,30}
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
        assert!(
            difficulty > 0.0,
            "difficulty must be positive, got {}",
            difficulty
        );
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

    /// LIVE staleness-TTL probe (run manually: `cargo test -- --ignored
    /// chain::tests::live_staleness_ttl_probe`). Requires a node on
    /// ws://127.0.0.1:9944 with a seeded feed. Verifies the new
    /// `head_block_number()` RPC and that `is_feed_stale` transitions from
    /// fresh to stale as the chain advances past the seeded `updated_at`.
    #[tokio::test]
    #[ignore]
    async fn live_staleness_ttl_probe() {
        let client = ChainClient::new("ws://127.0.0.1:9944").await.unwrap();
        let head = client.head_block_number().await.expect("head_block_number");
        let feed = client.get_feed().await.expect("seeded feed");
        println!(
            "[live] head_block = {}, feed.updated_at_block = {}",
            head, feed.updated_at_block
        );
        // Small TTL to observe the fresh->stale transition live (head advances
        // sub-second on dev).
        println!(
            "[live] stale(TTL=5)   = {}",
            is_feed_stale(feed.updated_at_block, head, Some(5))
        );
        println!(
            "[live] stale(TTL=1440)= {}",
            is_feed_stale(feed.updated_at_block, head, Some(1440))
        );
        assert!(
            head >= feed.updated_at_block,
            "head must not precede updated_at"
        );
    }
}
