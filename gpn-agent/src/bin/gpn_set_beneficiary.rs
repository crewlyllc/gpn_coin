//! gpn_set_beneficiary — point the block-reward beneficiary at a wallet via Sudo.
//!
//! Repoints `GPUPoW::BlockRewardBeneficiary` (the account that auto-receives each
//! mined block's 50 GPN reward) to an operator-chosen wallet. Root-gated, so it is
//! dispatched through `Sudo.sudo` with the sudo key.
//!
//! Usage:
//!   gpn_set_beneficiary [endpoint] [beneficiary_ss58] [sudo_uri]
//!
//! Defaults: endpoint = ws://127.0.0.1:9944, sudo_uri = //Alice

use gpn_agent::chain::signer;
use std::str::FromStr;
use subxt::dynamic::Value;
use subxt::tx::Signer as _;
use subxt::utils::AccountId32;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    let endpoint = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "ws://127.0.0.1:9944".to_string());
    let beneficiary = args
        .get(2)
        .cloned()
        .ok_or("usage: gpn_set_beneficiary [endpoint] <beneficiary_ss58> [sudo_uri]")?;
    let sudo_uri = args
        .get(3)
        .cloned()
        .unwrap_or_else(|| "//Alice".to_string());

    let beneficiary_addr = AccountId32::from_str(&beneficiary)?;

    println!("== gpn_set_beneficiary ==");
    println!("  endpoint:    {endpoint}");
    println!("  beneficiary: {beneficiary}");
    println!("  sudo:        {sudo_uri}");

    let client = signer::connect(&endpoint).await?;
    println!("Connected to GPN chain at {endpoint}");

    let sudo = signer::Sr25519Signer::from_uri(&sudo_uri)?;
    println!("Signing as sudo: {}", sudo.account_id());

    let inner = Value::unnamed_variant(
        "GPUPoW",
        vec![Value::unnamed_variant(
            "set_block_beneficiary",
            vec![Value::from_bytes(beneficiary_addr.as_ref() as &[u8; 32])],
        )],
    );
    let call = subxt::dynamic::tx("Sudo", "sudo", vec![inner]);

    let nonce = signer::current_nonce(&client, &endpoint, &sudo.account_id()).await;
    println!("Submitting Sudo.sudo(GPUPoW.set_block_beneficiary) [nonce {nonce}]...");
    signer::submit_signed(&client, &call, &sudo, nonce).await?;
    println!("✅ block-reward beneficiary set to {beneficiary}");

    Ok(())
}
