//! gpn_fail_job — fail a non-terminal marketplace job (refunds escrow).
//! Usage: gpn_fail_job [endpoint] [signer_uri] <job_id> [reason]
use gpn_agent::chain::ChainClient;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let endpoint = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "ws://127.0.0.1:9944".into());
    let signer_uri = args.get(2).cloned().unwrap_or_else(|| "//Alice".into());
    let job_id: u64 = args.get(3).ok_or("missing <job_id>")?.parse()?;
    let reason: u8 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(1);
    let client = ChainClient::new(&endpoint).await?.with_signer(&signer_uri);
    client.fail_job(job_id, reason).await?;
    println!("failed job #{job_id} (reason {reason})");
    Ok(())
}
