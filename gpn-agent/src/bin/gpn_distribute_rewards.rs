//! gpn_distribute_rewards — exercise the GPN fee economy end-to-end on-chain.
//!
//! The `gpn-token` pallet splits marketplace/transaction fees four ways
//! (staking pool 200 / treasury 150 / burn 100 / oracle pool 50 bps) via
//! `GPNToken::distribute_fees`, which is **root-only**. Because the GPN runtime
//! now includes the Sudo pallet, this tool dispatches that root call through
//! `Sudo.sudo` (signed by the sudo key, Alice on dev/local), then pays out the
//! accumulated pools:
//!   - `GPNToken::distribute_staking_rewards(stakers)` — tier-weighted payout
//!     (proof-of-staked-GPU: attested tier multiplies the share; unattested
//!     stakers are excluded).
//!   - `GPNToken::distribute_oracle_rewards(operators)` — even split.
//!
//! Because the GPN token pallet account starts empty (nothing feeds it yet —
//! marketplace fees currently land in the `gpn/cmkt` pallet account, not
//! `gpn/tokn`), pass an optional `seed_gpn` argument to first sudo
//! `Balances.force_set_balance` the token pallet account, simulating collected
//! fees so the pools are funded and the split/payout/burn is observable.
//!
//! Usage:
//!   gpn_distribute_rewards [endpoint] [total_fee_gpn] [sudo_uri] [stakers_csv_ss58] [operators_csv_ss58] [seed_gpn]
//!
//! Defaults (dev chain):
//!   endpoint      = ws://127.0.0.1:9944
//!   total_fee_gpn = 1000           (fee amount to distribute through the 4-way split)
//!   sudo_uri      = //Alice        (the dev/local sudo key)
//!   stakers_csv   = //Alice        (ss58 or dev URI; get paid proportional to attested tier)
//!   operators_csv = //Alice        (ss58 or dev URI; get paid an even split)
//!   seed_gpn      = 0              (if >0, sudo-force-set the token pallet balance first)
//!
//! Example live run against `gpn-node --dev`:
//!   cargo run -p gpn-agent --bin gpn_distribute_rewards --release
//!
//! The pools are also reported before/after so you can see real burn + payouts.

use gpn_agent::chain::signer;
use sp_core::crypto::Pair as _;
use subxt::dynamic::Value;
use subxt::tx::Signer as _;
use subxt::utils::AccountId32;
use std::str::FromStr;

/// Resolve an ss58 address or a dev URI (//Alice) to an AccountId32.
fn resolve_account(s: &str) -> Result<AccountId32, Box<dyn std::error::Error>> {
    if s.starts_with("//") {
        let pair = sp_core::sr25519::Pair::from_string(s, None)
            .map_err(|e| format!("invalid dev URI '{}': {}", s, e))?;
        Ok(AccountId32(pair.public().0))
    } else {
        Ok(AccountId32::from_str(s)?)
    }
}

/// Resolve the best head block hash (raw JSON-RPC) — `at_latest()` on a PoW
/// dev chain reads the stuck finalized #0 block, so we must read at head.
async fn head_hash(endpoint: &str) -> Option<subxt::utils::H256> {
    let http = endpoint.replace("ws://", "http://").replace("wss://", "https://");
    let body = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "chain_getBlockHash", "params": [] });
    let resp = reqwest::Client::new().post(&http).json(&body).send().await.ok()?;
    let v: serde_json::Value = resp.json().await.ok()?;
    let hex = v.get("result")?.as_str()?.trim_start_matches("0x");
    let bytes = hex::decode(hex).ok()?;
    if bytes.len() != 32 { return None; }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Some(arr.into())
}

/// Read a GPNToken storage value (StakingRewardPool / OracleRewardPool) as u128.
async fn read_pool(
    client: &subxt::OnlineClient<signer::GpnConfig>,
    endpoint: &str,
    storage_name: &str,
) -> u128 {
    let addr = subxt::dynamic::storage("GPNToken", storage_name, vec![]);
    let api = match client.storage().at_latest().await {
        Ok(a) => a,
        _ => return 0,
    };
    let api = match head_hash(endpoint).await {
        Some(h) => client.storage().at(h),
        None => api,
    };
    match api.fetch(&addr).await {
        Ok(Some(thunk)) => match thunk.to_value() {
            Ok(v) => v.remove_context().as_u128().unwrap_or(0),
            _ => 0,
        },
        _ => 0,
    }
}

/// Read a free balance for an AccountId (System::Account) at head.
async fn read_balance(
    client: &subxt::OnlineClient<signer::GpnConfig>,
    endpoint: &str,
    account: &AccountId32,
) -> u128 {
    let addr = subxt::dynamic::storage(
        "System",
        "Account",
        vec![Value::from_bytes((account.as_ref() as &[u8; 32]).to_vec())],
    );
    let api = match client.storage().at_latest().await {
        Ok(a) => a,
        _ => return 0,
    };
    let api = match head_hash(endpoint).await {
        Some(h) => client.storage().at(h),
        None => api,
    };
    let thunk = match api.fetch(&addr).await {
        Ok(Some(t)) => t,
        _ => return 0,
    };
    let v = match thunk.to_value() {
        Ok(v) => v.remove_context(),
        _ => return 0,
    };
    use subxt::ext::scale_value::ValueDef;
    if let ValueDef::Composite(c) = &v.value {
        if let subxt::ext::scale_value::Composite::Named(pairs) = c {
            if let Some((_, data)) = pairs.iter().find(|(k, _)| k == "data") {
                if let ValueDef::Composite(c2) = &data.value {
                    if let subxt::ext::scale_value::Composite::Named(p2) = c2 {
                        if let Some((_, free)) = p2.iter().find(|(k, _)| k == "free") {
                            return free.as_u128().unwrap_or(0);
                        }
                    }
                }
            }
        }
    }
    0
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    let endpoint = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "ws://127.0.0.1:9944".to_string());
    let total_fee_gpn: f64 = args
        .get(2)
        .map(|s| s.parse().unwrap_or(1000.0))
        .unwrap_or(1000.0);
    let sudo_uri = args
        .get(3)
        .cloned()
        .unwrap_or_else(|| "//Alice".to_string());
    let stakers_csv = args
        .get(4)
        .cloned()
        .unwrap_or_else(|| "//Alice".to_string());
    let operators_csv = args
        .get(5)
        .cloned()
        .unwrap_or_else(|| "//Alice".to_string());
    let seed_gpn: f64 = args
        .get(6)
        .map(|s| s.parse().unwrap_or(0.0))
        .unwrap_or(0.0);

    let total_fee_raw: u128 = (total_fee_gpn * 1_000_000_000_000.0) as u128; // 12 decimals

    let stakers: Vec<AccountId32> = stakers_csv
        .split(',')
        .map(|s| resolve_account(s.trim()))
        .collect::<Result<_, _>>()?;
    let operators: Vec<AccountId32> = operators_csv
        .split(',')
        .map(|s| resolve_account(s.trim()))
        .collect::<Result<_, _>>()?;

    println!("GPN fee-economy exercise on {} (sudo {})", endpoint, sudo_uri);
    println!(
        "  distribute_fees(total_fee={:.4} GPN) -> sudo.sudo  [200/150/100/50 bps: staking/treasury/burn/oracle]",
        total_fee_gpn
    );
    println!("  distribute_staking_rewards({} stakers)  [tier-weighted]", stakers.len());
    println!("  distribute_oracle_rewards({} operators) [even split]", operators.len());

    let client = signer::connect(&endpoint).await?;
    println!("Connected to GPN chain at {}", endpoint);

    let sudo = signer::Sr25519Signer::from_uri(&sudo_uri)?;
    println!("Signing as sudo: {}", sudo.account_id());

    // ---- 0. Optionally seed the token pallet account (simulate collected fees) ----
    // The `gpn/tokn` pallet account is where distribute_fees draws from. It
    // starts empty; marketplace fees currently go to `gpn/cmkt` instead, so
    // until that gap is closed we seed it here via sudo to make the economy
    // observable end-to-end.
    if seed_gpn > 0.0 {
        let seed_raw: u128 = (seed_gpn * 1_000_000_000_000.0) as u128;
        let pallet_acct = "5EYCAe5gvj12EgpmiPdyRWApUD3gpmy4swoxRVy1ooZ7eSST"; // gpn/tokn
        let pallet_addr = AccountId32::from_str(pallet_acct)?;
        let force_call = Value::unnamed_variant(
            "Balances",
            vec![Value::unnamed_variant(
                "force_set_balance",
                vec![
                    Value::from_bytes((pallet_addr.as_ref() as &[u8; 32]).to_vec()),
                    Value::u128(seed_raw),
                ],
            )],
        );
        let sudo_wrap = subxt::dynamic::tx("Sudo", "sudo", vec![force_call]);
        let nonce = signer::current_nonce(&client, &endpoint, &sudo.account_id()).await;
        println!(
            "  seeding token pallet account with {:.4} GPN via sudo...",
            seed_gpn
        );
        signer::submit_signed(&client, &sudo_wrap, &sudo, nonce).await?;
        println!("  ✅ token pallet account funded.");
    }

    // ---- Before ----
    let pool_before = read_pool(&client, &endpoint, "StakingRewardPool").await;
    let oracle_before = read_pool(&client, &endpoint, "OracleRewardPool").await;
    println!(
        "  [before] StakingRewardPool={:.4} GPN  OracleRewardPool={:.4} GPN",
        pool_before as f64 / 1e12,
        oracle_before as f64 / 1e12
    );

    // ---- 1. distribute_fees via Sudo.sudo (root dispatch) ----
    // Sudo.sudo takes a single RuntimeCall value: unnamed_variant(Pallet, [unnamed_variant(Call, [args])]).
    let inner_call = Value::unnamed_variant(
        "GPNToken",
        vec![Value::unnamed_variant("distribute_fees", vec![Value::u128(total_fee_raw)])],
    );
    let sudo_call = subxt::dynamic::tx("Sudo", "sudo", vec![inner_call]);
    let nonce = signer::current_nonce(&client, &endpoint, &sudo.account_id()).await;
    println!("  submitting Sudo.sudo(GPNToken.distribute_fees) [nonce {}]...", nonce);
    signer::submit_signed(&client, &sudo_call, &sudo, nonce).await?;
    println!("  ✅ distribute_fees finalized via sudo.");

    let pool_after = read_pool(&client, &endpoint, "StakingRewardPool").await;
    let oracle_after = read_pool(&client, &endpoint, "OracleRewardPool").await;
    println!(
        "  [after distribute_fees] StakingRewardPool={:.4} GPN (+{:.4})  OracleRewardPool={:.4} GPN (+{:.4})",
        pool_after as f64 / 1e12,
        (pool_after - pool_before) as f64 / 1e12,
        oracle_after as f64 / 1e12,
        (oracle_after - oracle_before) as f64 / 1e12
    );

    // ---- 2. distribute_staking_rewards ----
    let staker_values: Vec<Value> = stakers
        .iter()
        .map(|a| Value::from_bytes((a.as_ref() as &[u8; 32]).to_vec()))
        .collect();
    let stake_call = subxt::dynamic::tx(
        "GPNToken",
        "distribute_staking_rewards",
        vec![Value::unnamed_composite(staker_values)],
    );
    let payout_account = sudo.account_id();
    let nonce = signer::current_nonce(&client, &endpoint, &payout_account).await;
    println!("  submitting GPNToken.distribute_staking_rewards [nonce {}]...", nonce);
    signer::submit_signed(&client, &stake_call, &sudo, nonce).await?;
    println!("  ✅ staking rewards distributed.");

    // ---- 3. distribute_oracle_rewards ----
    let op_values: Vec<Value> = operators
        .iter()
        .map(|a| Value::from_bytes((a.as_ref() as &[u8; 32]).to_vec()))
        .collect();
    let oracle_call = subxt::dynamic::tx(
        "GPNToken",
        "distribute_oracle_rewards",
        vec![Value::unnamed_composite(op_values)],
    );
    let nonce = signer::current_nonce(&client, &endpoint, &payout_account).await;
    println!("  submitting GPNToken.distribute_oracle_rewards [nonce {}]...", nonce);
    signer::submit_signed(&client, &oracle_call, &sudo, nonce).await?;
    println!("  ✅ oracle rewards distributed.");

    // ---- After ----
    let pool_final = read_pool(&client, &endpoint, "StakingRewardPool").await;
    let oracle_final = read_pool(&client, &endpoint, "OracleRewardPool").await;
    println!(
        "  [after payouts] StakingRewardPool={:.4} GPN (drained: {})  OracleRewardPool={:.4} GPN (drained: {})",
        pool_final as f64 / 1e12,
        pool_final == 0,
        oracle_final as f64 / 1e12,
        oracle_final == 0
    );

    for s in &stakers {
        println!("  staker {} balance: {:.4} GPN", s, read_balance(&client, &endpoint, s).await as f64 / 1e12);
    }
    for o in &operators {
        println!("  operator {} balance: {:.4} GPN", o, read_balance(&client, &endpoint, o).await as f64 / 1e12);
    }

    Ok(())
}
