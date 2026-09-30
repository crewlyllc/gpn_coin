//! gpn_register — direct register_provider call (debug the hang).
//! Usage: gpn_register [endpoint] [signer_uri]

use gpn_agent::chain::signer;
use subxt::dynamic::Value;
use subxt::tx::Signer as _;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let endpoint = args.get(1).cloned().unwrap_or("ws://127.0.0.1:9944".into());
    let signer_uri = args.get(2).cloned().unwrap_or("//Alice".into());

    let client = signer::connect(&endpoint).await?;
    let signer = signer::Sr25519Signer::from_uri(&signer_uri)?;
    println!("signing as {}", signer.account_id());

    let req = Value::named_composite([
        ("min_vram_mb", Value::u128(8192)),
        ("min_compute_capability", Value::u128(89)),
        ("min_gpu_count", Value::u128(1)),
        ("min_cuda_cores", Value::u128(3072)),
        ("min_gflops", Value::u128(150)),
    ]);
    let call = subxt::dynamic::tx("ComputeMarketplace", "register_provider", vec![req]);
    let nonce = signer::current_nonce(&client, &endpoint, &signer.account_id()).await;
    println!("submitting register_provider [nonce {nonce}]...");
    match signer::submit_signed(&client, &call, &signer, nonce).await {
        Ok(()) => println!("✅ registered"),
        Err(e) => println!("❌ error: {e}"),
    }
    Ok(())
}
