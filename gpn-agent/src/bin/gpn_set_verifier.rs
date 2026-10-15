//! gpn_set_verifier — set the mutable GPUAttestation challenge-verifier account
//! via `sudo(GPUAttestation::set_challenge_verifier)`. Root-only; lets the
//! verifier key be rotated without a WASM upgrade (key-leak response).
//!
//! Usage:
//!   gpn_set_verifier [endpoint] [new_ss58|0xhex] [sudo_uri]
//!   gpn_set_verifier [endpoint]                      # read-only: print current storage
//!
//! Reads the raw `GPUAttestation::ChallengeVerifierAccount` storage at the best
//! block (twox128 key) before/after.

use gpn_agent::chain::signer;
use std::str::FromStr;
use subxt::dynamic::Value;
use subxt::tx::Signer as _;
use subxt::utils::AccountId32;

fn storage_key() -> String {
    use sp_core::hashing::twox_128;
    let mut k = Vec::with_capacity(32);
    k.extend_from_slice(&twox_128(b"GPUAttestation"));
    k.extend_from_slice(&twox_128(b"ChallengeVerifierAccount"));
    format!("0x{}", hex::encode(k))
}

async fn read_verifier(endpoint: &str) -> Option<[u8; 32]> {
    let http = endpoint
        .replace("ws://", "http://")
        .replace("wss://", "https://");
    let client = reqwest::Client::new();
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
    let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"state_getStorage","params":[storage_key(), head]});
    let resp: serde_json::Value = client
        .post(&http)
        .json(&body)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let raw = match resp.get("result").and_then(|r| r.as_str()) {
        Some(r) => r.trim_start_matches("0x").to_string(),
        None => return None, // empty storage (falls back to compiled-in default)
    };
    let bytes = hex::decode(raw).ok()?;
    let out: [u8; 32] = bytes.try_into().ok()?;
    Some(out)
}

fn ss58(bytes: &[u8; 32]) -> String {
    use sp_core::crypto::Ss58Codec;
    sp_core::sr25519::Public::from_raw(*bytes).to_ss58check()
}

fn parse_account(raw: &str) -> Result<[u8; 32], String> {
    let t = raw.trim();
    if t.starts_with("0x") || t.starts_with("0X") {
        let body = &t[2..];
        if body.len() != 64 {
            return Err(format!("hex account must be 64 chars (got {})", body.len()));
        }
        let b = hex::decode(body).map_err(|e| format!("invalid hex: {e}"))?;
        let mut out = [0u8; 32];
        out.copy_from_slice(&b);
        Ok(out)
    } else {
        Ok(AccountId32::from_str(t)
            .map_err(|e| format!("invalid ss58 '{t}': {e}"))?
            .0)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let endpoint = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "ws://127.0.0.1:9944".into());
    let new_account = args.get(2).cloned();
    let sudo_uri = args.get(3).cloned().unwrap_or_else(|| "//Alice".into());

    println!("== gpn_set_verifier ==");
    println!("  endpoint: {endpoint}");
    match read_verifier(&endpoint).await {
        Some(b) => println!(
            "  ChallengeVerifierAccount (storage): {} (0x{})",
            ss58(&b),
            hex::encode(b)
        ),
        None => println!("  ChallengeVerifierAccount (storage): <empty → compiled-in default>"),
    }

    let new_account = match new_account {
        None => return Ok(()),
        Some(a) => a,
    };
    let nb = parse_account(&new_account)?;
    println!("  new verifier: {} (0x{})", ss58(&nb), hex::encode(nb));

    let client = signer::connect(&endpoint).await?;
    let sudo = signer::Sr25519Signer::from_uri(&sudo_uri)?;
    println!("  sudo: {}", sudo.account_id());

    let inner = Value::unnamed_variant(
        "GPUAttestation",
        vec![Value::unnamed_variant(
            "set_challenge_verifier",
            vec![Value::from_bytes(&nb)],
        )],
    );
    let call = subxt::dynamic::tx("Sudo", "sudo", vec![inner]);
    if std::env::var("GPN_DUMP_CALL").is_ok() {
        use subxt::tx::Payload;
        let md = client.metadata();
        let mut buf = Vec::new();
        call.encode_call_data_to(&md, &mut buf)?;
        println!("call_data: 0x{}", hex::encode(&buf));
        return Ok(());
    }
    let nonce = signer::current_nonce(&client, &endpoint, &sudo.account_id()).await;
    println!("Submitting Sudo.sudo(GPUAttestation.set_challenge_verifier) [nonce {nonce}]...");
    signer::submit_signed(&client, &call, &sudo, nonce).await?;
    tokio::time::sleep(std::time::Duration::from_secs(8)).await;
    match read_verifier(&endpoint).await {
        Some(b) if b == nb => println!("✅ VERIFIED: verifier storage == {}", ss58(&b)),
        Some(b) => eprintln!("⚠ storage now {} (expected {})", ss58(&b), ss58(&nb)),
        None => eprintln!("⚠ storage still empty after set"),
    }
    Ok(())
}
