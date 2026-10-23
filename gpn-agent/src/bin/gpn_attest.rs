//! gpn_attest — stake + attest a GPU provider (Bronze tier) for the marketplace.
//!
//! Usage: gpn_attest [endpoint] [signer_uri] [stake_gpn] [gflops]
//!   endpoint   ws://127.0.0.1:9944 (default)
//!   signer_uri //Alice (default)
//!   stake_gpn  1000 (default; must be >= MinAttestationStake = 1000 GPN)
//!   gflops     188  (default; the 4060's measured throughput)

use gpn_agent::chain::signer;
use subxt::dynamic::Value;
use subxt::tx::Signer as _;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    let endpoint = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "ws://127.0.0.1:9944".to_string());
    let signer_uri = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "//Alice".to_string());
    let stake_gpn: u128 = args
        .get(3)
        .map(|s| s.parse().unwrap_or(1000))
        .unwrap_or(1000);
    let gflops: u64 = args.get(4).map(|s| s.parse().unwrap_or(188)).unwrap_or(188);

    let stake_raw = stake_gpn * 1_000_000_000_000; // 12 decimals

    println!("== gpn_attest ==");
    println!("  endpoint:  {endpoint}");
    println!("  signer:    {signer_uri}");
    println!("  stake:     {stake_gpn} GPN");
    println!("  gflops:    {gflops}");

    let client = signer::connect(&endpoint).await?;
    let signer = signer::Sr25519Signer::from_uri(&signer_uri)?;
    println!("Signing as: {}", signer.account_id());

    // 1. deposit_stake
    let stake_call = subxt::dynamic::tx(
        "GPUAttestation",
        "deposit_stake",
        vec![Value::u128(stake_raw)],
    );
    let nonce = signer::current_nonce(&client, &endpoint, &signer.account_id()).await;
    println!("Submitting deposit_stake [nonce {nonce}]...");
    signer::submit_signed(&client, &stake_call, &signer, nonce).await?;
    println!("✅ staked {stake_gpn} GPN");

    // 2. submit_attestation
    let challenge = [0u8; 32];
    let gpu_hash = [0u8; 32];
    let attest_call = subxt::dynamic::tx(
        "GPUAttestation",
        "submit_attestation",
        vec![
            Value::from_bytes(challenge),
            Value::from_bytes(gpu_hash),
            Value::u128(1),              // gpu_count
            Value::u128(gflops as u128), // compute_gflops
        ],
    );
    let nonce = signer::current_nonce(&client, &endpoint, &signer.account_id()).await;
    println!("Submitting submit_attestation [nonce {nonce}]...");
    signer::submit_signed(&client, &attest_call, &signer, nonce).await?;
    println!("✅ attested (Bronze, {gflops} GFLOPS)");

    Ok(())
}
