//! gpn_two_account_demo — production-realistic REQUESTOR ≠ PROVIDER flow.
//!
//! The original marketplace demo (`gpn_marketplace_demo`) uses a single account
//! for both sides. This one proves the real multi-party escrow/payment path:
//!
//!   Requestor (`//Bob`)  creates a paid job  (escrow debited from Bob)
//!   Provider  (`//Alice`) serves it          (payout credited to Alice)
//!
//! No GPU is required — the provider submits a deterministic result hash, so
//! this runs against a plain CPU `gpn-node --dev --alice --bob`. It verifies
//! on-chain that:
//!   - job is Completed
//!   - `provider` == Alice (NOT Bob) and `requestor` == Bob (NOT Alice)
//!   - Alice's balance rose by ~price×(1 − fee)  (fee = MarketplaceFeeBps = 5%)
//!   - Bob's   balance fell by exactly price     (escrow)
//!
//! Usage:
//!   gpn_two_account_demo [endpoint] [requestor_uri] [provider_uri] [price_gpn] [min_gflops]
//!   defaults: ws://127.0.0.1:9944  //Bob  //Alice  50  0
//!
//! `min_gflops` defaults to 0 (open market, no attestation gate) so the demo
//! needs no prior GPU attestation. Set it > 0 to exercise the gate (the
//! provider must then have attested to that many GFLOPS first — see the
//! `gpn_marketplace_demo`/cycle-27 attest flow).

use gpn_agent::chain::ChainClient;
use sha2::{Digest, Sha256};

const FEE_BPS: u64 = 500; // MarketplaceFeeBps = 5% (runtime)
const GPN_UNITS: f64 = 1e12; // 1 GPN = 1e12 base units

fn parse_u64(s: &str, default: u64) -> u64 {
    s.parse().unwrap_or(default)
}
fn parse_f64(s: &str, default: f64) -> f64 {
    s.parse().unwrap_or(default)
}

fn print_bal(label: &str, info: &gpn_agent::chain::AccountInfo) {
    println!("    {:<10} {:>12.6} GPN  (nonce {})", label, info.balance, info.nonce);
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    let endpoint = args.get(1).cloned().unwrap_or_else(|| "ws://127.0.0.1:9944".to_string());
    let requestor_uri = args.get(2).cloned().unwrap_or_else(|| "//Bob".to_string());
    let provider_uri = args.get(3).cloned().unwrap_or_else(|| "//Alice".to_string());
    let price_gpn = parse_f64(args.get(4).map(|s| s.as_str()).unwrap_or("50"), 50.0);
    let min_gflops = parse_u64(args.get(5).map(|s| s.as_str()).unwrap_or("0"), 0);

    let price: u128 = (price_gpn * GPN_UNITS) as u128;
    println!("=== GPN two-account marketplace demo (requestor ≠ provider) ===");
    println!("endpoint={} requestor={} provider={} price={} GPN min_gflops={}",
        endpoint, requestor_uri, provider_uri, price_gpn, min_gflops);

    let req = ChainClient::new(&endpoint).await?.with_signer(&requestor_uri);
    let prov = ChainClient::new(&endpoint).await?.with_signer(&provider_uri);

    // Resolve addresses from the signers (deterministic from URI).
    let (req_addr, prov_addr) = {
        use subxt::tx::Signer as _;
        let r = gpn_agent::chain::signer::Sr25519Signer::from_uri(&requestor_uri)?;
        let p = gpn_agent::chain::signer::Sr25519Signer::from_uri(&provider_uri)?;
        (r.account_id().to_string(), p.account_id().to_string())
    };
    // Hex (lowercase, no 0x) form of the resolved account ids — matches the
    // format `get_job_state` returns for `JobRequestor`/`JobProvider`.
    let (req_hex, prov_hex) = {
        use subxt::tx::Signer as _;
        let r = gpn_agent::chain::signer::Sr25519Signer::from_uri(&requestor_uri)?;
        let p = gpn_agent::chain::signer::Sr25519Signer::from_uri(&provider_uri)?;
        (format!("0x{}", hex::encode(r.account_id().0)), format!("0x{}", hex::encode(p.account_id().0)))
    };

    println!("\n[0] balances BEFORE");
    let req_before = req.get_account_info(&req_addr).await?;
    let prov_before = prov.get_account_info(&prov_addr).await?;
    print_bal(&format!("requestor({})", short(&requestor_uri)), &req_before);
    print_bal(&format!("provider({})", short(&provider_uri)), &prov_before);

    // 1. Requestor (Bob) posts a paid job → escrow debited from Bob.
    let workload_hash = [0xabu8; 32];
    println!("\n[1] {} creates job (LLMInference, Spot, price={} GPN, min_gflops={})",
        short(&requestor_uri), price_gpn, min_gflops);
    req.create_job(8_000, 75, 1, 0, min_gflops, "LLMInference", "Spot", price, 100, workload_hash).await?;
    let next_id = req.get_next_job_id().await;
    let job_id = next_id.saturating_sub(1);
    println!("    ✅ job #{} created by requestor", job_id);
    // Confirm escrow left the requestor immediately.
    let req_mid = req.get_account_info(&req_addr).await?;
    println!("    requestor balance now {:.6} GPN (escrow {:.6} GPN debited)",
        req_mid.balance, req_before.balance - req_mid.balance);

    // 2. Provider (Alice) registers (idempotent).
    println!("\n[2] {} registers as provider", short(&provider_uri));
    if !prov.is_provider_registered(&prov_addr).await {
        prov.register_provider(8_000, 75, 1, 0, min_gflops).await?;
        println!("    ✅ provider registered");
    } else {
        println!("    ✅ already registered (skipped)");
    }

    // 3. Provider accepts → starts → submits → completes.
    println!("\n[3] {} accepts job #{}", short(&provider_uri), job_id);
    prov.accept_job(job_id).await?;
    if let Some((state, _, _, _)) = prov.get_job_state(job_id).await {
        println!("    ✅ state = {}", state);
    }

    println!("[4] {} starts job #{}", short(&provider_uri), job_id);
    prov.start_job(job_id).await?;

    // Deterministic result hash (no GPU needed for this demo).
    let result_hash: [u8; 32] = Sha256::digest(b"gpn-two-account-demo-result").into();
    println!("[5] {} submits result 0x{}...", short(&provider_uri), hex::encode(&result_hash[..4]));
    prov.submit_result(job_id, result_hash).await?;

    println!("[6] {} completes job #{} (payout from escrow)", short(&provider_uri), job_id);
    prov.complete_job(job_id).await?;

    // 4. Verify final state + balances.
    let (state, job_price, job_req, job_prov) = prov.get_job_state(job_id).await
        .ok_or("job not found after completion")?;
    let req_after = req.get_account_info(&req_addr).await?;
    let prov_after = prov.get_account_info(&prov_addr).await?;

    println!("\n[7] final job state");
    println!("    job #{}: state={} price={:.6} GPN", job_id, state, job_price as f64 / GPN_UNITS);
    println!("    requestor on-chain = {}", job_req.clone().unwrap_or_default());
    println!("    provider  on-chain = {}", job_prov.clone().unwrap_or_default());

    println!("\n[8] balances AFTER");
    print_bal(&format!("requestor({})", short(&requestor_uri)), &req_after);
    print_bal(&format!("provider({})", short(&provider_uri)), &prov_after);

    // 5. Assertions.
    let fee = price * FEE_BPS as u128 / 10_000;
    let expect_prov_gain = (price - fee) as f64 / GPN_UNITS;
    let prov_gain = prov_after.balance - prov_before.balance;
    let req_loss = req_before.balance - req_after.balance;
    let expect_req_loss = price as f64 / GPN_UNITS;

    let mut ok = true;
    let mut check = |name: &str, cond: bool| {
        println!("    {} {}", if cond { "✅" } else { "❌" }, name);
        ok &= cond;
    };

    check("job is Completed", state == "Completed");
    check("provider is the provider account", job_prov.as_deref() == Some(prov_hex.as_str()));
    check("requestor is the requestor account", job_req.as_deref() == Some(req_hex.as_str()));
    check(
        &format!("provider earned ~{:.6} GPN (price − 5% fee)", expect_prov_gain),
        (prov_gain - expect_prov_gain).abs() < 0.01,
    );
    check(
        &format!("requestor lost ~{:.6} GPN (escrow)", expect_req_loss),
        (req_loss - expect_req_loss).abs() < 0.01,
    );

    println!("\n{}", if ok {
        "=== TWO-ACCOUNT REQUESTOR→PROVIDER FLOW VERIFIED ✅ ==="
    } else {
        "=== TWO-ACCOUNT FLOW: ASSERTION(S) FAILED ❌ ==="
    });
    Ok(())
}

fn short(uri: &str) -> String {
    uri.trim_start_matches("//").to_string()
}
