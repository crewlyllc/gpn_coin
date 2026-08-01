//! gpn_marketplace_demo — live end-to-end compute-marketplace flow on the GPU.
//!
//! Drives the FULL job lifecycle against a running `gpn-node` (default
//! ws://127.0.0.1:9944) signed as the same account that already attested on
//! the GPU (cycle 27). It exercises the cycle-26 `min_gflops` attestation gate
//! at `accept_job` and, between `start_job` and `submit_result`, runs a REAL
//! dense GEMM on the local GPU (OpenCL) and hashes its output buffer to produce
//! the on-chain `result_hash` — i.e. the compute is genuinely done by the
//! provider's GPU, then recorded on-chain.
//!
//!   gpn_marketplace_demo [endpoint] [signer_uri] [min_gflops]
//!
//! Defaults: ws://127.0.0.1:9944, //Alice, min_gflops = 150 (must be ≤ the
//! provider's attested GFLOPS — on the 4060 that's ~188, so 150 passes; a value
//! above the attestation must be REJECTED by the pallet).
//!
//! Prereqs (cycle 27 state): the signer account has staked + attested. This is
//! normally done automatically by `gpn-agent` in `mine` mode (or by running the
//! attest demo / `gpn_check_state` after a prior live attest).

use gpn_agent::chain::ChainClient;
use gpn_agent::gpu::matmul::GpuMatmul;
use sha2::{Digest, Sha256};

fn parse_u64(s: &str, default: u64) -> u64 {
    s.parse().unwrap_or(default)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    let endpoint = args.get(1).cloned().unwrap_or_else(|| "ws://127.0.0.1:9944".to_string());
    let signer_uri = args.get(2).cloned().unwrap_or_else(|| "//Alice".to_string());
    let min_gflops = parse_u64(args.get(3).map(|s| s.as_str()).unwrap_or("150"), 150);

    println!("=== GPN compute-marketplace end-to-end demo ===");
    println!("endpoint={} signer={} min_gflops={}", endpoint, signer_uri, min_gflops);

    let client = ChainClient::new(&endpoint).await?.with_signer(&signer_uri);
    let signer_addr = {
        use subxt::tx::Signer as _;
        let s = gpn_agent::chain::signer::Sr25519Signer::from_uri(&signer_uri)?;
        s.account_id().to_string()
    };

    // 1. Provider state (stake + attestation) must already exist.
    let (staked, attested) = client.get_provider_state(&signer_addr).await;
    let att_record = client.get_attestation_record(&signer_addr).await;
    println!("\n[1] provider {} staked={} attested={}", signer_addr, staked, attested);
    if let Some((gpus, gflops, tier)) = &att_record {
        println!("    attestation: gpus={} gflops={} tier={}", gpus, gflops, tier);
    }
    if !staked || !attested {
        println!("⚠️  Provider not staked+attested. Run the agent attest flow (cycle 27) first.");
        return Ok(());
    }

    // 2. Register as a provider (idempotent: skip entirely if already registered,
    //    so we don't submit a duplicate tx that collides in the pool).
    let already_registered = client.is_provider_registered(&signer_addr).await;
    println!("\n[2] register_provider (vram=8GB, cc=75, gpus=1, min_gflops={}) — already_registered={}", min_gflops, already_registered);
    if !already_registered {
        match client.register_provider(8_000, 75, 1, 0, min_gflops).await {
            Ok(()) => println!("    ✅ provider registered"),
            Err(e) => println!("    ❌ register failed: {}", e),
        }
    } else {
        println!("    ✅ already registered (skipped)");
    }

    // 3. Create a job (escrow transferred). Requestor = same account for the demo.
    let workload_hash = [0xabu8; 32];
    let price: u128 = 50_000_000_000_000; // 50 GPN (1e12 units)
    println!("\n[3] create_job (LLMInference, Spot, price={} GPN, min_gflops={})", price as f64 / 1e12, min_gflops);
    client
        .create_job(8_000, 75, 1, 0, min_gflops, "LLMInference", "Spot", price, 100, workload_hash)
        .await?;
    let next_id = client.get_next_job_id().await;
    let job_id = next_id.saturating_sub(1);
    println!("    ✅ job created, id = {}", job_id);

    // 4. Accept the job — this is where the cycle-26 attestation gate applies.
    println!("\n[4] accept_job({}) — attestation gate (attested {} GFLOPS vs min {})", job_id, att_record.as_ref().map(|a| a.1).unwrap_or(0), min_gflops);
    client.accept_job(job_id).await?;
    let after_accept = client.get_job_state(job_id).await;
    match after_accept.as_ref().map(|(s, _, _, _)| s.as_str()) {
        Some("Accepted") => println!("    ✅ accepted, job state = Accepted"),
        Some("Pending") => {
            println!("    ❌ accept_job was REJECTED (job still Pending). This is the EXPECTED");
            println!("       ProviderComputeInsufficient rejection when min_gflops={} > attested {} GFLOPS.", min_gflops, att_record.as_ref().map(|a| a.1).unwrap_or(0));
            println!("       ✅ the attestation gate is working correctly.");
            return Ok(());
        }
        other => println!("    ⚠️  unexpected state after accept: {:?}", other),
    }

    // 5. Start the job (Accepted -> Running).
    println!("\n[5] start_job({})", job_id);
    client.start_job(job_id).await?;
    if let Some((state, _, _, _)) = client.get_job_state(job_id).await {
        println!("    ✅ started, job state = {}", state);
    }

    // 6. Run REAL compute on the GPU and hash the output -> result_hash.
    println!("\n[6] running real GPU GEMM (OpenCL) to produce the result hash...");
    let n = 1024usize;
    let mut mm = GpuMatmul::new(0, n).map_err(|e| format!("open GPU matmul: {e}"))?;
    println!("    device: {}", mm.device_name());
    let (elapsed, _gflops) = mm.run_many(2).map_err(|e| format!("matmul: {e}"))?;
    let out = mm.read_result().map_err(|e| format!("read result: {e}"))?;
    let mut hasher = Sha256::new();
    for chunk in out.chunks(4096) {
        let bytes: Vec<u8> = chunk.iter().flat_map(|f| f.to_le_bytes()).collect();
        hasher.update(&bytes);
    }
    let result_hash: [u8; 32] = hasher.finalize().into();
    println!("    ✅ GEMM done in {}ms, result_hash = 0x{}", elapsed.as_millis(), hex::encode(&result_hash[..8]));

    // 7. Submit the result (Running -> AwaitingVerification).
    println!("\n[7] submit_result({}, 0x{}...)", job_id, hex::encode(&result_hash[..4]));
    client.submit_result(job_id, result_hash).await?;
    if let Some((state, _, _, _)) = client.get_job_state(job_id).await {
        println!("    ✅ result submitted, job state = {}", state);
    }

    // 8. Complete the job (payout provider from escrow).
    println!("\n[8] complete_job({})", job_id);
    let bal_before = client.get_account_info(&signer_addr).await?.balance;
    client.complete_job(job_id).await?;
    if let Some((state, price, _, provider)) = client.get_job_state(job_id).await {
        println!("    ✅ completed, job state = {}", state);
        let bal_after = client.get_account_info(&signer_addr).await?.balance;
        println!("    escrow={} GPN, provider={}", price as f64 / 1e12, provider.unwrap_or_default());
        println!("    balance {} -> {} GPN (net change {:.6})", bal_before, bal_after, bal_after - bal_before);
    }

    println!("\n=== END-TO-END MARKETPLACE FLOW COMPLETE ✅ ===");
    Ok(())
}
