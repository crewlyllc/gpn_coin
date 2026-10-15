//! gpn_set_block_reward — set the GPU-PoW block reward via sudo (Root-only).
//! Usage: gpn_set_block_reward [endpoint] [sudo_uri] <gpn>
use gpn_agent::chain::signer;
use subxt::dynamic::Value;
use subxt::tx::Signer as _;

const GPN_UNITS: u128 = 1_000_000_000_000;

/// Read `GPUPoW::CurrentBlockReward` at the **best** block (the finalized head
/// is stuck at genesis on this PoW chain, so `at_latest` can't be used).
async fn read_reward(
    client: &subxt::OnlineClient<signer::GpnConfig>,
    endpoint: &str,
) -> Option<u128> {
    let http = endpoint
        .replace("ws://", "http://")
        .replace("wss://", "https://");
    let body =
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"chain_getBlockHash","params":[]});
    let resp: serde_json::Value = reqwest::Client::new()
        .post(&http)
        .json(&body)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let hex = resp.get("result")?.as_str()?.trim_start_matches("0x");
    let bytes = hex::decode(hex).ok()?;
    let arr: [u8; 32] = bytes.try_into().ok()?;
    let head: subxt::utils::H256 = arr.into();
    let addr = subxt::dynamic::storage("GPUPoW", "CurrentBlockReward", vec![]);
    let s = client.storage().at(head);
    let t = s.fetch(&addr).await.ok()??;
    t.to_value().ok()?.as_u128()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let endpoint = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "ws://127.0.0.1:9944".into());
    let sudo_uri = args.get(2).cloned().unwrap_or_else(|| "//Alice".into());
    let gpn: u128 = args.get(3).ok_or("missing <gpn>")?.parse()?;
    let raw = gpn.saturating_mul(GPN_UNITS);

    let client = signer::connect(&endpoint).await?;
    let sudo = signer::Sr25519Signer::from_uri(&sudo_uri)?;
    println!(
        "current block reward (raw): {:?}",
        read_reward(&client, &endpoint).await
    );

    let inner = Value::unnamed_variant(
        "GPUPoW",
        vec![Value::unnamed_variant(
            "set_block_reward",
            vec![Value::u128(raw)],
        )],
    );
    let call = subxt::dynamic::tx("Sudo", "sudo", vec![inner]);
    let nonce = signer::current_nonce(&client, &endpoint, &sudo.account_id()).await;
    println!("sudo set_block_reward({gpn} GPN = {raw} raw) [nonce {nonce}]...");
    signer::submit_signed(&client, &call, &sudo, nonce).await?;
    println!("✅ submitted");
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
    println!(
        "block reward now (raw): {:?}",
        read_reward(&client, &endpoint).await
    );
    Ok(())
}
