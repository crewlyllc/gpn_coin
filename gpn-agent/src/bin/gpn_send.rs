//! gpn_send — one-shot native GPN transfer (ops tool).
//!
//! Funds an account (e.g. a fresh offline key that needs gas) from a funded
//! signer. Uses `Balances.transfer_allow_death`.
//!
//! Usage:
//!   gpn_send [endpoint] [signer_uri] <dest_ss58|0x-hex> <amount_gpn>
//! Env: ENDPOINT, SIGNER_URI.

use gpn_agent::chain::signer;
use std::str::FromStr;
use subxt::dynamic::Value;
use subxt::tx::Signer as _;
use subxt::utils::AccountId32;

const GPN_UNITS: f64 = 1_000_000_000_000.0;

fn parse_dest(raw: &str) -> Result<[u8; 32], String> {
    let t = raw.trim();
    if t.starts_with("0x") || t.starts_with("0X") {
        let body = &t[2..];
        if body.len() != 64 {
            return Err(format!("hex address must be 64 chars (got {})", body.len()));
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
        .or_else(|| std::env::var("ENDPOINT").ok())
        .unwrap_or_else(|| "ws://127.0.0.1:9944".to_string());
    let signer_uri = args
        .get(2)
        .cloned()
        .or_else(|| std::env::var("SIGNER_URI").ok())
        .unwrap_or_else(|| "//Alice".to_string());
    let dest = args.get(3).ok_or("missing <dest>")?;
    let amount_gpn: f64 = args
        .get(4)
        .ok_or("missing <amount_gpn>")?
        .parse()
        .map_err(|e| format!("bad amount: {e}"))?;

    let dest_bytes = parse_dest(dest)?;
    let amount_raw: u128 = (amount_gpn * GPN_UNITS) as u128;

    let client = signer::connect(&endpoint).await?;
    let sudo = signer::Sr25519Signer::from_uri(&signer_uri)?;
    let call = subxt::dynamic::tx(
        "Balances",
        "transfer_allow_death",
        vec![Value::from_bytes(&dest_bytes), Value::u128(amount_raw)],
    );
    let nonce = signer::current_nonce(&client, &endpoint, &sudo.account_id()).await;
    println!(
        "sending {amount_gpn} GPN from {} to {dest} (nonce {nonce})...",
        sudo.account_id()
    );
    signer::submit_signed(&client, &call, &sudo, nonce).await?;
    println!("✅ sent {amount_gpn} GPN → {dest}");
    Ok(())
}
