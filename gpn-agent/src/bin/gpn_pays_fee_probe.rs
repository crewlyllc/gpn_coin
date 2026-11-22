//! gpn_pays_fee_probe — prove from the chain, not from source, that
//! `ComputeMarketplace::register_provider` (call index 0) is free to submit.
//!
//! It signs the call offline for `signer_uri` and submits it for real. The
//! transaction pool decides `pays_fee` from the call's `DispatchInfo` before
//! dispatch, so a sender holding only **1 planck** can only be *included* if the
//! annotation is `Pays::No`; with `Pays::Yes` the pool returns
//! `InvalidTransaction` — "Inability to pay some fees". Pair a run before and
//! after the runtime upgrade, and check the sender's balance with
//! `state_getStorage System.Account` to show nothing was charged.
//!
//! Usage: gpn_pays_fee_probe [endpoint] [signer_uri]
//!        GPN_TRACE_TX=1 prints the exact extrinsic handed to the pool.

use gpn_agent::chain::signer;
use subxt::dynamic::Value;
use subxt::tx::Signer as _;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let args: Vec<String> = std::env::args().collect();
    let endpoint = args.get(1).cloned().unwrap_or("ws://127.0.0.1:9944".into());
    let uri = args.get(2).cloned().unwrap_or("//Alice".into());

    let client = signer::connect(&endpoint).await?;
    let who = signer::Sr25519Signer::from_uri(&uri)?;
    let acct = who.account_id();

    // GPURequirements { min_vram_mb u32, min_compute_capability u16,
    //                  min_gpu_count u8, min_cuda_cores u32, min_gflops u64 }
    let reqs = Value::unnamed_composite(vec![
        Value::from(0u32),
        Value::from(0u16),
        Value::from(0u8),
        Value::from(0u32),
        Value::from(0u64),
    ]);
    let call = subxt::dynamic::tx("ComputeMarketplace", "register_provider", vec![reqs]);

    let nonce = signer::current_nonce(&client, &endpoint, &acct).await;
    println!("signer : {acct} ({uri})");
    println!("nonce  : {nonce}");
    println!("submitting ComputeMarketplace::register_provider ...");

    match signer::submit_signed(&client, &call, &who, nonce).await {
        Ok(()) => {
            println!("✅ included in a best block and dispatched OK");
            println!("   (a sender that could not pay would have been refused by the pool)");
            Ok(())
        }
        Err(e) => {
            println!("❌ {e}");
            Err(e)
        }
    }
}
