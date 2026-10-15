//! gpn_set_sudo — rotate the chain Sudo (`Sudo::Key`) to a new account via
//! `sudo::set_key`, signed by the CURRENT sudo key (Root-only path).
//!
//! Security use: a leaked sudo key must be replaced. This submits
//! `Sudo.sudo(Sudo.set_key(<new>))` signed by the old key, then reads back
//! `Sudo::Key` at the best block to verify the rotation landed.
//!
//! Usage:
//!   gpn_set_sudo [endpoint] [old_sudo_uri] [new_account_ss58|0xhex]
//!   gpn_set_sudo [endpoint] [old_sudo_uri]              # read-only: print current key
//!
//! Defaults: endpoint = ws://127.0.0.1:9944
//!
//! Reading uses the **best** block (finalized head is stuck at genesis on this
//! PoW testnet), same approach as gpn_set_block_reward.

use gpn_agent::chain::signer;
use std::str::FromStr;
use subxt::dynamic::Value;
use subxt::tx::Signer as _;
use subxt::utils::AccountId32;

/// Read `Sudo::Key` at the **best** block, via raw `state_getStorage` (twox128
/// storage key) so we don't depend on subxt dynamic Value byte decoding.
async fn read_sudo_key(endpoint: &str) -> Option<[u8; 32]> {
    use sp_core::hashing::twox_128;
    let http = endpoint
        .replace("ws://", "http://")
        .replace("wss://", "https://");
    let client = reqwest::Client::new();

    // best block hash
    let body =
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"chain_getBlockHash","params":[]});
    let resp: serde_json::Value = client
        .post(&http)
        .json(&body)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let head = resp.get("result")?.as_str()?.to_string();

    // twox128("Sudo") ++ twox128("Key")
    let mut key = Vec::with_capacity(32);
    key.extend_from_slice(&twox_128(b"Sudo"));
    key.extend_from_slice(&twox_128(b"Key"));
    let key_hex = format!("0x{}", hex::encode(key));

    let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"state_getStorage","params":[key_hex, head]});
    let resp: serde_json::Value = client
        .post(&http)
        .json(&body)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let raw = resp.get("result")?.as_str()?.trim_start_matches("0x");
    let bytes = hex::decode(raw).ok()?;
    let out: [u8; 32] = bytes.try_into().ok()?;
    Some(out)
}

fn ss58(bytes: &[u8; 32]) -> String {
    use sp_core::crypto::Ss58Codec;
    sp_core::sr25519::Public::from_raw(*bytes).to_ss58check()
}

/// Parse an ss58 or 0x-hex account into raw bytes.
fn parse_account(raw: &str) -> Result<[u8; 32], String> {
    let t = raw.trim();
    if t.starts_with("0x") || t.starts_with("0X") {
        let body = &t[2..];
        if body.len() != 64 {
            return Err(format!("hex account must be 64 chars (got {})", body.len()));
        }
        let bytes = hex::decode(body).map_err(|e| format!("invalid hex: {e}"))?;
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        Ok(out)
    } else {
        let acct = AccountId32::from_str(t).map_err(|e| format!("invalid ss58 '{t}': {e}"))?;
        Ok(acct.0)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let endpoint = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "ws://127.0.0.1:9944".into());
    let old_sudo_uri = args
        .get(2)
        .cloned()
        .ok_or("usage: gpn_set_sudo [endpoint] <old_sudo_uri> [new_account_ss58|0xhex]")?;
    let new_account = args.get(3).cloned();

    println!("== gpn_set_sudo ==");
    println!("  endpoint: {endpoint}");
    println!("  old sudo: {old_sudo_uri}");

    let client = signer::connect(&endpoint).await?;
    let old_sudo = signer::Sr25519Signer::from_uri(&old_sudo_uri)?;
    println!("  old sudo account: {}", old_sudo.account_id());

    let cur = read_sudo_key(&endpoint).await;
    match cur {
        Some(b) => println!("  Sudo::Key (before): {} (0x{})", ss58(&b), hex::encode(b)),
        None => println!("  Sudo::Key (before): <unreadable>"),
    }

    let new_account = match new_account {
        None => {
            println!("no <new_account> given — read-only; exiting");
            return Ok(());
        }
        Some(a) => a,
    };
    let new_bytes = parse_account(&new_account)?;
    let new_ss58 = ss58(&new_bytes);
    println!("  new account: {new_ss58} (0x{})", hex::encode(new_bytes));

    let inner = Value::unnamed_variant(
        "Sudo",
        vec![Value::unnamed_variant(
            "set_key",
            vec![Value::from_bytes(&new_bytes)],
        )],
    );
    let call = subxt::dynamic::tx("Sudo", "sudo", vec![inner]);
    let nonce = signer::current_nonce(&client, &endpoint, &old_sudo.account_id()).await;
    println!("Submitting Sudo.sudo(Sudo.set_key) [nonce {nonce}]...");
    signer::submit_signed(&client, &call, &old_sudo, nonce).await?;
    println!("✅ submitted — waiting for inclusion...");
    tokio::time::sleep(std::time::Duration::from_secs(8)).await;

    let after = read_sudo_key(&endpoint).await;
    match after {
        Some(b) if b == new_bytes => {
            println!(
                "✅ VERIFIED: Sudo::Key == new account {} (0x{})",
                ss58(&b),
                hex::encode(b)
            );
        }
        Some(b) => {
            eprintln!(
                "❌ Sudo::Key still {} (0x{}) — rotation NOT confirmed",
                ss58(&b),
                hex::encode(b)
            );
            std::process::exit(2);
        }
        None => {
            eprintln!("⚠ could not re-read Sudo::Key after submit");
            std::process::exit(3);
        }
    }
    Ok(())
}
