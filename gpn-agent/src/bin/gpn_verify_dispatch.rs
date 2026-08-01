//! gpn_verify_dispatch — verify the Cycle-38/39 robustness fix: `signer::submit_signed`
//! now returns `Err` when an extrinsic is *included but fails dispatch*, instead of
//! the old behavior of returning `Ok` on mere inclusion (which logged "succeeded"
//! for a failed tx — that masked the `MinAttestationStake` unit bug in cycle 38).
//!
//! It submits an extrinsic that is **guaranteed to fail dispatch**:
//!   `ComputeMarketplace::accept_job(job_id)` on a non-existent job → the pallet
//!   returns `Error::JobNotFound`. The tx is still included in a block (with a
//!   `System::ExtrinsicFailed` event), so this is a perfect probe:
//!   - OLD code: returns Ok("tx included in a best block") — a false success.
//!   - NEW code: returns Err("tx included but FAILED dispatch: ... JobNotFound").
//!
//! Exit code 0 = fix verified (dispatch failure correctly surfaced as Err).
//! Exit code 1 = FAILURE (submit_signed returned Ok on a failing dispatch).
//!
//! Usage:
//!   gpn_verify_dispatch [endpoint] [signer_uri]
//! Defaults: ws://127.0.0.1:9944, //Alice

use gpn_agent::chain::signer;
use subxt::dynamic::Value;
use subxt::tx::Signer as _;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let endpoint = args.get(1).cloned().unwrap_or_else(|| "ws://127.0.0.1:9944".into());
    let uri = args.get(2).cloned().unwrap_or_else(|| "//Alice".into());

    let client = signer::connect(&endpoint).await?;
    let signer = signer::Sr25519Signer::from_uri(&uri)?;
    let account = signer.account_id();
    let nonce = signer::current_nonce(&client, &endpoint, &account).await;

    // Non-existent job id — accept_job must fail dispatch with JobNotFound.
    let bad_job_id: u128 = 999_999_999;
    let call = subxt::dynamic::tx(
        "ComputeMarketplace",
        "accept_job",
        vec![Value::u128(bad_job_id)],
    );

    println!("Submitting ComputeMarketplace::accept_job({bad_job_id}) as {uri} ...");
    println!("  (job {bad_job_id} does not exist → dispatch MUST fail with JobNotFound)");

    match signer::submit_signed(&client, &call, &signer, nonce).await {
        Ok(_) => {
            eprintln!(
                "\n❌ FIX NOT VERIFIED: submit_signed returned Ok on a tx that FAILED dispatch.\n\
                 This is the exact cycle-38 bug (inclusion ≠ success)."
            );
            std::process::exit(1);
        }
        Err(e) => {
            let msg = format!("{e}");
            println!("\nsubmit_signed returned Err: {msg}");
            // The dispatch failure must be surfaced as Err AND carry a resolvable
            // module error name (pallet::error). `accept_job` checks provider
            // registration before job existence, so either ProviderNotRegistered
            // or JobNotFound is a valid "failed dispatch" outcome here.
            if msg.contains("FAILED dispatch") && msg.contains("ModuleError(") {
                println!("\n✅ FIX VERIFIED: failed dispatch correctly surfaced as Err with a resolvable module error.");
            } else {
                println!("\n⚠️  submit_signed returned Err but without the expected resolvable module error:");
                println!("   {msg}");
                std::process::exit(1);
            }
        }
    }
    Ok(())
}
