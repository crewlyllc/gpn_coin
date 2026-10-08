//! gpn_probe_root_gate — live probe that the fee-economy distribution calls are
//! **root-only** on the running chain (spec-13 item #11 theft-vector fix).
//!
//! Defect being verified: `GPNToken::distribute_staking_rewards` and
//! `distribute_oracle_rewards` take a **caller-supplied recipient list**. Before
//! the fix they were guarded only by `ensure_signed`, so *any* signed account
//! could pass a one-address list and drain the whole pool to itself. The fix
//! (`dd36e8f`) put `ensure_root` on both, matching the sibling root-only
//! `distribute_fees` / `set_fee_distribution`.
//!
//! This probe submits each call **signed directly** (NOT wrapped in
//! `Sudo.sudo`) and asserts dispatch fails with `BadOrigin`. It passes an
//! **empty** recipient list, so even if the gate were missing the calls are
//! no-ops (`operators/stakers.is_empty()` returns early) ⇒ the probe is
//! non-destructive either way.
//!
//! Usage:
//!   gpn_probe_root_gate [endpoint] [signer_uri]
//! Defaults: ws://127.0.0.1:9944  //Alice
//!
//! Exit code 0 = both calls rejected with BadOrigin (gate live).
//! Exit code 1 = a call was accepted while signed (gate MISSING) or submit errored otherwise.

use gpn_agent::chain::signer;
use subxt::dynamic::Value;
use subxt::tx::Signer as _;

async fn expect_bad_origin(
    client: &subxt::OnlineClient<signer::GpnConfig>,
    endpoint: &str,
    signer: &signer::Sr25519Signer,
    pallet: &str,
    call: &str,
) -> bool {
    let tx = subxt::dynamic::tx(pallet, call, vec![Value::unnamed_composite(vec![])]);
    let nonce = signer::current_nonce(client, endpoint, &signer.account_id()).await;
    println!("  submitting SIGNED {pallet}.{call}([]) [nonce {nonce}] (expect BadOrigin)...");
    match signer::submit_signed(client, &tx, signer, nonce).await {
        Ok(()) => {
            println!("  ❌ {pallet}.{call} was ACCEPTED while signed — gate MISSING!");
            false
        }
        Err(e) => {
            let s = e.to_string();
            let low = s.to_lowercase();
            // `signer::describe_module_error` renders a non-module DispatchError
            // as `DispatchError(variant=N bytes=0x...)`. `sp_runtime::DispatchError`
            // variant 2 == `BadOrigin` (see signer.rs:760). A Module error (variant 3)
            // renders as `ModuleError(...)`, so this is unambiguous.
            let is_bad_origin = low.contains("badorigin")
                || low.contains("bad origin")
                || low.contains("dispatcherror(variant=2 ");
            if is_bad_origin {
                println!("  ✅ {pallet}.{call} rejected: {s}");
                true
            } else {
                println!("  ⚠ {pallet}.{call} failed, but NOT with BadOrigin: {s}");
                false
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let endpoint = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "ws://127.0.0.1:9944".into());
    let uri = args.get(2).cloned().unwrap_or_else(|| "//Alice".into());

    let client = signer::connect(&endpoint).await?;
    let signer = signer::Sr25519Signer::from_uri(&uri)?;
    println!(
        "root-gate probe on {endpoint} (signed caller {}) — empty recipient lists (non-destructive)",
        signer.account_id()
    );

    let a = expect_bad_origin(
        &client,
        &endpoint,
        &signer,
        "GPNToken",
        "distribute_oracle_rewards",
    )
    .await;
    let b = expect_bad_origin(
        &client,
        &endpoint,
        &signer,
        "GPNToken",
        "distribute_staking_rewards",
    )
    .await;

    if a && b {
        println!("RESULT: PASS — both distributions are root-only (item #11 fix live).");
        Ok(())
    } else {
        println!("RESULT: FAIL — a distribution call was reachable while signed.");
        std::process::exit(1);
    }
}
