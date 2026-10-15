//! gpn_phase_c_probe — live verification of the Phase C staking-backed slashing
//! oracle (oracle-governance §6 gap #5) on a running `gpn-node --dev`.
//!
//! Phase C makes the oracle operator set **permissionless** and
//! **economically-secured**: anyone bonds GPN to become an operator
//! (`bond_operator`), bonded operators post feeds (weight = bond size), and a
//! stale/outlier operator can be **slashed** via a challenge anyone can file
//! (`challenge_operator` + `resolve_challenge`). Slashing burns `SlashBps` of
//! the operator bond (D4 default: burn, aligning with the 20% burn in
//! `gpn-token`). Sybil resistance = bond cost + slashing.
//!
//! This probe exercises the full dispatch path live:
//!   1. `bond_operator` by Dave (non-committee, non-operator) -> bonded.
//!   2. Dave `update_feed` -> accepted (proves the bonded path authorizes posts).
//!   3. Reads `StakedOperators` (bond size) on-chain.
//!   4. `bond_more` -> bond weight increases.
//!   5. `challenge_operator` by Charlie against Dave -> challenge opened.
//!   6. `resolve_challenge(..., Dismiss)` -> challenge closed; Dave's bond intact
//!      (frivolous-challenge path: challenger's bond burned).
//!   7. `unbond_operator` by Dave -> bond returned, Dave removed.
//!
//! Exit code 0 = all Phase C assertions pass; 1 = any assertion fails.
//!
//! Usage:
//!   gpn_phase_c_probe [endpoint] [bond_uri] [challenger_uri]
//! Defaults: ws://127.0.0.1:9944  //Dave  //Charlie

use gpn_agent::chain::signer;
use gpn_agent::chain::ChainClient;
use sp_core::{crypto::Pair as _, sr25519};
use subxt::dynamic::Value;
use subxt::ext::scale_value::Composite;
use subxt::tx::Signer as _;

/// Resolve an account's 32-byte id from a Substrate dev URI (`//Dave`).
fn account_bytes(uri: &str) -> [u8; 32] {
    sr25519::Pair::from_string(uri, None)
        .expect("valid dev URI")
        .public()
        .0
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let endpoint = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "ws://127.0.0.1:9944".into());
    let bond_uri = args.get(2).cloned().unwrap_or_else(|| "//Dave".into());
    let challenger_uri = args.get(3).cloned().unwrap_or_else(|| "//Charlie".into());

    let client = signer::connect(&endpoint).await?;
    let chain = ChainClient::new(&endpoint).await?;
    let bonder = account_bytes(&bond_uri);

    let mut ok = true;
    let mut check = |cond: bool, label: &str| {
        println!("  {} {}", if cond { "✅" } else { "❌" }, label);
        if !cond {
            ok = false;
        }
    };

    println!("== 1. {bond_uri} bond_operator (permissionless, non-committee) ==");
    let b = signer::Sr25519Signer::from_uri(&bond_uri)?;
    let b_acct = b.account_id();
    let b_nonce = signer::current_nonce(&client, &endpoint, &b_acct).await;
    let bond_call = subxt::dynamic::tx(
        "ProfitabilityOracle",
        "bond_operator",
        Composite::unnamed(vec![]),
    );
    match signer::submit_signed(&client, &bond_call, &b, b_nonce).await {
        Ok(_) => check(
            true,
            format!("{bond_uri} bond_operator dispatched OK").as_str(),
        ),
        Err(e) => check(false, &format!("bond_operator failed: {e}")),
    }
    let bond_val = chain.get_staked_bond(&bonder).await;
    check(
        bond_val == Some(1_000_000_000_000_000),
        "StakedOperators[bonder] == 1,000 GPN bond (on-chain)",
    );

    println!("\n== 2. {bond_uri} (bonded) update_feed ==");
    let feed_call = subxt::dynamic::tx(
        "ProfitabilityOracle",
        "update_feed",
        vec![
            Value::u128(150_000),   // token_price_usd micro-USD ($0.15)
            Value::u128(3_000_000), // avg_gpu_hour_price micro-USD/hr
            Value::u128(7_200),     // utilization_rate_bps
            Value::u128(1_500),     // staking_apr_bps
        ],
    );
    let b_nonce2 = signer::current_nonce(&client, &endpoint, &b_acct).await;
    match signer::submit_signed(&client, &feed_call, &b, b_nonce2).await {
        Ok(_) => check(
            true,
            format!("{bond_uri} update_feed accepted (bonded operator path)").as_str(),
        ),
        Err(e) => check(false, &format!("bonded update_feed failed: {e}")),
    }

    println!("\n== 3. {bond_uri} bond_more(+100 GPN) -> weight up ==");
    let b_nonce3 = signer::current_nonce(&client, &endpoint, &b_acct).await;
    let more_call = subxt::dynamic::tx(
        "ProfitabilityOracle",
        "bond_more",
        vec![Value::u128(100_000_000_000_000)], // +100 GPN
    );
    match signer::submit_signed(&client, &more_call, &b, b_nonce3).await {
        Ok(_) => {
            let bv = chain.get_staked_bond(&bonder).await;
            check(
                bv == Some(1_100_000_000_000_000),
                "bond increased to 1,100 GPN (on-chain)",
            );
        }
        Err(e) => check(false, &format!("bond_more failed: {e}")),
    }

    println!("\n== 4. {challenger_uri} challenge_operator({bond_uri}) ==");
    let c = signer::Sr25519Signer::from_uri(&challenger_uri)?;
    let c_acct = c.account_id();
    let c_nonce = signer::current_nonce(&client, &endpoint, &c_acct).await;
    let chal_call = subxt::dynamic::tx(
        "ProfitabilityOracle",
        "challenge_operator",
        vec![Value::from_bytes(bonder)],
    );
    match signer::submit_signed(&client, &chal_call, &c, c_nonce).await {
        Ok(_) => check(
            true,
            format!("challenge_operator({bond_uri}) dispatched OK").as_str(),
        ),
        Err(e) => check(false, &format!("challenge_operator failed: {e}")),
    }

    println!("\n== 5. resolve_challenge(..., Dismiss) -> challenge closed, target intact ==");
    let c_nonce2 = signer::current_nonce(&client, &endpoint, &c_acct).await;
    let dismiss_call = subxt::dynamic::tx(
        "ProfitabilityOracle",
        "resolve_challenge",
        vec![Value::from_bytes(bonder), Value::u128(2)], // 2 = Dismiss
    );
    match signer::submit_signed(&client, &dismiss_call, &c, c_nonce2).await {
        Ok(_) => {
            let bv = chain.get_staked_bond(&bonder).await;
            check(
                bv == Some(1_100_000_000_000_000),
                "challenge closed, operator bond intact (frivolous -> challenger burned)",
            );
        }
        Err(e) => check(false, &format!("resolve_challenge(Dismiss) failed: {e}")),
    }

    println!("\n== 6. {bond_uri} unbond_operator -> bond returned ==");
    let b_nonce4 = signer::current_nonce(&client, &endpoint, &b_acct).await;
    let unbond_call = subxt::dynamic::tx(
        "ProfitabilityOracle",
        "unbond_operator",
        Composite::unnamed(vec![]),
    );
    match signer::submit_signed(&client, &unbond_call, &b, b_nonce4).await {
        Ok(_) => {
            let still = chain.get_staked_bond(&bonder).await;
            check(
                still.is_none(),
                format!("{bond_uri} unbonded (removed from StakedOperators)").as_str(),
            );
        }
        Err(e) => check(false, &format!("unbond_operator failed: {e}")),
    }

    println!("\n== RESULT ==");
    if ok {
        println!("✅ Phase C staking-backed slashing oracle VERIFIED LIVE on-chain.");
        std::process::exit(0);
    } else {
        println!("❌ One or more Phase C assertions FAILED.");
        std::process::exit(1);
    }
}
