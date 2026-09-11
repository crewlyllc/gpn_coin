//! gpn_committee_probe — live verification of Phase B committee admission
//! (oracle-governance §6 gap #4) on a running `gpn-node --dev`.
//!
//! Phase B replaces root-only `add_operator` with a **named committee** that
//! votes to admit new oracle operators: any committee member may
//! `propose_operator(op)` (self-approving); once `AdmissionThreshold`
//! approvals accumulate via `approve_operator`, the operator is admitted.
//! This removes the single-root capture point for the risky direction (adding
//! operators to the trusted feed-writer set) without adding a new pallet.
//!
//! This probe exercises the full dispatch path live:
//!   1. Reads `Committee` / `OracleOperators` membership.
//!   2. Committee member A `propose_operator(C)` -> pending proposal stored.
//!   3. Committee member B `approve_operator(C)` -> threshold reached, C admitted.
//!   4. C (now an operator) `update_feed` -> accepted (proves real operator).
//!   5. Negative: a NON-committee member's `propose_operator` fails dispatch
//!      with `NotCommitteeMember`.
//!
//! Exit code 0 = all Phase-B assertions pass; 1 = any assertion fails.
//!
//! Usage:
//!   gpn_committee_probe [endpoint] [proposer_uri] [approver_uri] [new_op_uri]
//! Defaults: ws://127.0.0.1:9944  //Alice  //Bob  //Charlie

use gpn_agent::chain::signer;
use sp_core::{crypto::Pair as _, sr25519};
use subxt::dynamic::Value;
use subxt::tx::Signer as _;

/// Resolve an account's 32-byte id from a Substrate dev URI (`//Alice`).
fn account_bytes(uri: &str) -> [u8; 32] {
    sr25519::Pair::from_string(uri, None)
        .expect("valid dev URI")
        .public()
        .0
}

/// Fetch a dynamic storage map entry at the **head** block, returning whether
/// the key is present. Mirrors `signer::current_nonce`'s head-hash resolution
/// (finalized is stuck at #0 on a PoW dev chain, so we read best-head).
async fn map_contains(
    client: &subxt::OnlineClient<signer::GpnConfig>,
    endpoint: &str,
    pallet: &str,
    item: &str,
    key: &[u8; 32],
) -> Option<bool> {
    let addr = subxt::dynamic::storage(pallet, item, vec![Value::from_bytes(key)]);
    let http_url = endpoint
        .replace("ws://", "http://")
        .replace("wss://", "https://");
    let body = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "chain_getBlockHash", "params": [] });
    let head_hash: Option<subxt::utils::H256> = {
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
        Some(thunk)
    };
    fetch.await.map(|_| true)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let endpoint = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "ws://127.0.0.1:9944".into());
    let proposer_uri = args.get(2).cloned().unwrap_or_else(|| "//Alice".into());
    let approver_uri = args.get(3).cloned().unwrap_or_else(|| "//Bob".into());
    let new_op_uri = args.get(4).cloned().unwrap_or_else(|| "//Charlie".into());

    let client = signer::connect(&endpoint).await?;
    let new_op = account_bytes(&new_op_uri);
    let alice = account_bytes("//Alice");
    let bob = account_bytes("//Bob");
    let dave = account_bytes("//Dave");

    let mut ok = true;
    let mut check = |cond: bool, label: &str| {
        println!("  {} {}", if cond { "✅" } else { "❌" }, label);
        if !cond {
            ok = false;
        }
    };

    println!("== 1. Committee membership (Phase B genesis) ==");
    check(
        map_contains(
            &client,
            &endpoint,
            "ProfitabilityOracle",
            "Committee",
            &alice,
        )
        .await
            == Some(true),
        "Alice is a committee member",
    );
    check(
        map_contains(&client, &endpoint, "ProfitabilityOracle", "Committee", &bob).await
            == Some(true),
        "Bob is a committee member",
    );
    check(
        map_contains(
            &client,
            &endpoint,
            "ProfitabilityOracle",
            "Committee",
            &dave,
        )
        .await
            != Some(true),
        "Dave is NOT a committee member",
    );

    println!("\n== 2. {proposer_uri} proposes {new_op_uri} as operator ==");
    let p = signer::Sr25519Signer::from_uri(&proposer_uri)?;
    let p_acct = p.account_id();
    let p_nonce = signer::current_nonce(&client, &endpoint, &p_acct).await;
    let propose_call = subxt::dynamic::tx(
        "ProfitabilityOracle",
        "propose_operator",
        vec![Value::from_bytes(new_op)],
    );
    match signer::submit_signed(&client, &propose_call, &p, p_nonce).await {
        Ok(_) => {
            check(
                true,
                format!("propose_operator({new_op_uri}) dispatched OK").as_str(),
            );
            let pending = map_contains(
                &client,
                &endpoint,
                "ProfitabilityOracle",
                "PendingAdmissions",
                &new_op,
            )
            .await;
            check(
                pending == Some(true),
                format!("pending admission stored for {new_op_uri} (threshold not yet met)")
                    .as_str(),
            );
        }
        Err(e) => check(false, &format!("propose_operator failed: {e}")),
    }

    println!("\n== 3. {approver_uri} approves -> threshold met, admitted ==");
    let a = signer::Sr25519Signer::from_uri(&approver_uri)?;
    let a_acct = a.account_id();
    let a_nonce = signer::current_nonce(&client, &endpoint, &a_acct).await;
    let approve_call = subxt::dynamic::tx(
        "ProfitabilityOracle",
        "approve_operator",
        vec![Value::from_bytes(new_op)],
    );
    match signer::submit_signed(&client, &approve_call, &a, a_nonce).await {
        Ok(_) => {
            check(
                true,
                format!("approve_operator({new_op_uri}) dispatched OK").as_str(),
            );
            let in_ops = map_contains(
                &client,
                &endpoint,
                "ProfitabilityOracle",
                "OracleOperators",
                &new_op,
            )
            .await;
            check(
                in_ops == Some(true),
                format!("{new_op_uri} is now an oracle operator").as_str(),
            );
            let still_pending = map_contains(
                &client,
                &endpoint,
                "ProfitabilityOracle",
                "PendingAdmissions",
                &new_op,
            )
            .await;
            check(
                still_pending != Some(true),
                "pending admission cleared after admission",
            );
        }
        Err(e) => check(false, &format!("approve_operator failed: {e}")),
    }

    println!("\n== 4. {new_op_uri} (now an operator) updates the feed ==");
    let c = signer::Sr25519Signer::from_uri(&new_op_uri)?;
    let c_acct = c.account_id();
    let c_nonce = signer::current_nonce(&client, &endpoint, &c_acct).await;
    let feed_call = subxt::dynamic::tx(
        "ProfitabilityOracle",
        "update_feed",
        vec![
            Value::u128(75_000),    // token_price_usd micro-USD
            Value::u128(3_000_000), // avg_gpu_hour_price micro-USD/hr
            Value::u128(7_200),     // utilization_rate_bps
            Value::u128(1_500),     // staking_apr_bps
        ],
    );
    match signer::submit_signed(&client, &feed_call, &c, c_nonce).await {
        Ok(_) => check(
            true,
            format!("{new_op_uri} update_feed accepted (real operator)").as_str(),
        ),
        Err(e) => check(
            false,
            &format!("{new_op_uri} update_feed failed (should be operator now): {e}"),
        ),
    }

    println!("\n== 5. Negative: non-committee member Dave proposes ==");
    let d = signer::Sr25519Signer::from_uri("//Dave")?;
    let d_acct = d.account_id();
    let d_nonce = signer::current_nonce(&client, &endpoint, &d_acct).await;
    let dave_call = subxt::dynamic::tx(
        "ProfitabilityOracle",
        "propose_operator",
        vec![Value::from_bytes(new_op)],
    );
    match signer::submit_signed(&client, &dave_call, &d, d_nonce).await {
        Ok(_) => check(false, "Dave's propose_operator should have FAILED dispatch"),
        Err(e) => {
            let msg = format!("{e}");
            let expected = msg.contains("FAILED dispatch") && msg.contains("NotCommitteeMember");
            check(
                expected,
                &format!("Dave rejected with NotCommitteeMember (got: {msg})"),
            );
        }
    }

    println!("\n== RESULT ==");
    if ok {
        println!("✅ Phase B committee admission VERIFIED LIVE on-chain.");
        std::process::exit(0);
    } else {
        println!("❌ One or more Phase B assertions FAILED.");
        std::process::exit(1);
    }
}
