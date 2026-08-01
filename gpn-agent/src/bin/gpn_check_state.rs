//! gpn_check_state — read on-chain provider state for an account (stake + attestation).
use gpn_agent::chain::ChainClient;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = std::env::args().nth(1).unwrap_or_else(|| "ws://127.0.0.1:9944".into());
    let addr = std::env::args().nth(2).unwrap_or_else(|| "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY".into());
    let client = ChainClient::new(&endpoint).await?;
    let nonce = client.get_account_nonce(&addr).await;
    let (staked, attested) = client.get_provider_state(&addr).await;
    let ai = client.get_account_info(&addr).await?;
    println!("account={} nonce={} balance={:.4} GPN staked={} attested={}", addr, nonce, ai.balance, staked, attested);

    // Read the attestation record details (gpu_count, compute_gflops, tier).
    if let Some((gpus, gflops, tier)) = client.get_attestation_record(&addr).await {
        println!("attestation: gpus={} gflops={} tier={}", gpus, gflops, tier);
    }
    Ok(())
}
