//! gpn_create_job — post a marketplace job and leave it Pending so the agent
//! (auto mode) can claim & serve it. This is the requestor-side of the
//! loop-closing test: the agent should pick up this job on-chain.
use gpn_agent::chain::ChainClient;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let endpoint = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "ws://127.0.0.1:9944".to_string());
    let signer_uri = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "//Alice".to_string());
    let min_gflops: u64 = args.get(3).map(|s| s.parse().unwrap_or(150)).unwrap_or(150);

    let client = ChainClient::new(&endpoint).await?.with_signer(&signer_uri);
    let workload_hash = [0xabu8; 32];
    let price: u128 = 50_000_000_000_000; // 50 GPN
    println!(
        "creating job (min_gflops={}, price=50 GPN) — leaving Pending for the agent to claim",
        min_gflops
    );
    client
        .create_job(
            8_000,
            75,
            1,
            0,
            min_gflops,
            "LLMInference",
            "Spot",
            price,
            100,
            workload_hash,
        )
        .await?;
    let next = client.get_next_job_id().await;
    let id = next.saturating_sub(1);
    println!("✅ job #{} created (state=Pending)", id);
    Ok(())
}
