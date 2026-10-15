//! gpn_upgrade_runtime — apply a FORKLESS runtime upgrade via `sudo(System.set_code)`.
//!
//! This is the production-correct way to ship a runtime change: the new WASM
//! replaces the on-chain `:code` while the chain KEEPS RUNNING — no genesis
//! change, no reset, no re-seed, all state preserved. (Contrast with regenerating
//! the committed-WASM + resetting, which is only a testnet shortcut.)
//!
//! Usage:
//!   gpn_upgrade_runtime [endpoint] [wasm_path] [sudo_uri]
//! Defaults: ws://127.0.0.1:9944,
//!           gpn-node/runtime/gpn_runtime.compact.compressed.wasm, //Alice

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
    let wasm_path = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "gpn-node/runtime/gpn_runtime.compact.compressed.wasm".to_string());
    let sudo_uri = args
        .get(3)
        .cloned()
        .unwrap_or_else(|| "//Alice".to_string());

    let wasm = std::fs::read(&wasm_path)?;
    println!("== gpn_upgrade_runtime ==");
    println!("  endpoint: {endpoint}");
    println!("  wasm:     {wasm_path} ({} bytes)", wasm.len());

    let client = signer::connect(&endpoint).await?;
    let sudo = signer::Sr25519Signer::from_uri(&sudo_uri)?;
    println!("Signing as sudo: {}", sudo.account_id());

    let inner = Value::unnamed_variant(
        "System",
        vec![Value::unnamed_variant(
            "set_code",
            vec![Value::from_bytes(&wasm)],
        )],
    );
    let call = subxt::dynamic::tx("Sudo", "sudo", vec![inner]);
    let nonce = signer::current_nonce(&client, &endpoint, &sudo.account_id()).await;
    println!("Submitting Sudo.sudo(System.set_code) [nonce {nonce}]...");
    signer::submit_signed(&client, &call, &sudo, nonce).await?;
    println!("✅ runtime upgraded in place (forkless) — chain continued, state preserved");
    Ok(())
}
