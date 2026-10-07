//! gpn_channel — off-chain escrow channel CLI (item #2,
//! `design-offchain-escrow-channels.md`).
//!
//! Wraps the agent's channel manager (`gpn_agent::chain::channel`) so the two
//! channel parties can:
//!   * `open`   — lock escrow on-chain for a `(requestor → provider)` channel
//!   * `sign`   — produce this party's authorization over an off-chain state
//!   * `close`  — cooperative close, submitting both parties' signatures
//!   * `force-close` — settle after the TTL with the counterparty's signature
//!
//! `sign` prints the 32-byte payload and 64-byte signature (hex) so the two
//! parties can exchange them out-of-band (or via an AP hub) and then anyone can
//! submit `close`.
//!
//! Usage:
//!   gpn_channel open        <endpoint> <requestor_uri> <provider_ss58> <amount> <ttl>
//!   gpn_channel sign        <signer_uri> <channel_id> <bal_requestor> <bal_provider> <nonce>
//!   gpn_channel close       <endpoint> <submitter_uri> <channel_id> <bal_requestor> <bal_provider> <nonce> <sig_requestor_hex> <sig_provider_hex>
//!   gpn_channel force-close <endpoint> <submitter_uri> <channel_id> <bal_requestor> <bal_provider> <nonce> <sig_counterparty_hex>

use gpn_agent::chain::channel::channel_settle_payload;
use gpn_agent::chain::signer::Sr25519Signer;
use gpn_agent::chain::ChainClient;
use sp_core::crypto::Ss58Codec;

fn hex64(s: &str) -> Result<[u8; 64], Box<dyn std::error::Error>> {
    let bytes = hex::decode(s.trim_start_matches("0x"))?;
    let mut out = [0u8; 64];
    if bytes.len() != 64 {
        return Err(format!("expected 64-byte signature, got {} bytes", bytes.len()).into());
    }
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn provider_pub(ss58: &str) -> Result<[u8; 32], Box<dyn std::error::Error>> {
    Ok(sp_core::sr25519::Public::from_ss58check(ss58)
        .map_err(|e| format!("invalid provider ss58 '{}': {:?}", ss58, e))?
        .0)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("help");

    match cmd {
        "open" => {
            let endpoint = args.get(2).ok_or("missing <endpoint>")?;
            let requestor_uri = args.get(3).ok_or("missing <requestor_uri>")?;
            let provider = provider_pub(args.get(4).ok_or("missing <provider_ss58>")?)?;
            let amount: u128 = args.get(5).ok_or("missing <amount>")?.parse()?;
            let ttl: u32 = args.get(6).ok_or("missing <ttl>")?.parse()?;
            let client = ChainClient::new(endpoint).await?.with_signer(requestor_uri);
            client.open_channel(provider, amount, ttl).await?;
            println!("✅ open_channel submitted (amount={amount}, ttl={ttl} blocks)");
        }
        "sign" => {
            let signer_uri = args.get(2).ok_or("missing <signer_uri>")?;
            let channel_id: u64 = args.get(3).ok_or("missing <channel_id>")?.parse()?;
            let br: u128 = args.get(4).ok_or("missing <bal_requestor>")?.parse()?;
            let bp: u128 = args.get(5).ok_or("missing <bal_provider>")?.parse()?;
            let nonce: u64 = args.get(6).ok_or("missing <nonce>")?.parse()?;
            let payload = channel_settle_payload(channel_id, br, bp, nonce);
            let signer = Sr25519Signer::from_uri(signer_uri)?;
            let sig = signer.sign_raw(&payload);
            println!("payload {}", hex::encode(payload));
            println!("sig     {}", hex::encode(sig));
        }
        "close" => {
            let endpoint = args.get(2).ok_or("missing <endpoint>")?;
            let submitter_uri = args.get(3).ok_or("missing <submitter_uri>")?;
            let channel_id: u64 = args.get(4).ok_or("missing <channel_id>")?.parse()?;
            let br: u128 = args.get(5).ok_or("missing <bal_requestor>")?.parse()?;
            let bp: u128 = args.get(6).ok_or("missing <bal_provider>")?.parse()?;
            let nonce: u64 = args.get(7).ok_or("missing <nonce>")?.parse()?;
            let sig_requestor = hex64(args.get(8).ok_or("missing <sig_requestor_hex>")?)?;
            let sig_provider = hex64(args.get(9).ok_or("missing <sig_provider_hex>")?)?;
            let client = ChainClient::new(endpoint).await?.with_signer(submitter_uri);
            client
                .close_channel(channel_id, br, bp, nonce, sig_requestor, sig_provider)
                .await?;
            println!("✅ close_channel submitted (channel {channel_id}, nonce {nonce})");
        }
        "force-close" => {
            let endpoint = args.get(2).ok_or("missing <endpoint>")?;
            let submitter_uri = args.get(3).ok_or("missing <submitter_uri>")?;
            let channel_id: u64 = args.get(4).ok_or("missing <channel_id>")?.parse()?;
            let br: u128 = args.get(5).ok_or("missing <bal_requestor>")?.parse()?;
            let bp: u128 = args.get(6).ok_or("missing <bal_provider>")?.parse()?;
            let nonce: u64 = args.get(7).ok_or("missing <nonce>")?.parse()?;
            let sig_counterparty = hex64(args.get(8).ok_or("missing <sig_counterparty_hex>")?)?;
            let client = ChainClient::new(endpoint).await?.with_signer(submitter_uri);
            client
                .force_close_channel(channel_id, br, bp, nonce, sig_counterparty)
                .await?;
            println!("✅ force_close_channel submitted (channel {channel_id}, nonce {nonce})");
        }
        _ => {
            eprintln!(
                "gpn_channel — off-chain escrow channel CLI\n\n\
                 Usage:\n\
                 gpn_channel open        <endpoint> <requestor_uri> <provider_ss58> <amount> <ttl>\n\
                 gpn_channel sign        <signer_uri> <channel_id> <bal_requestor> <bal_provider> <nonce>\n\
                 gpn_channel close       <endpoint> <submitter_uri> <channel_id> <br> <bp> <nonce> <sig_requestor_hex> <sig_provider_hex>\n\
                 gpn_channel force-close <endpoint> <submitter_uri> <channel_id> <br> <bp> <nonce> <sig_counterparty_hex>"
            );
            std::process::exit(1);
        }
    }
    Ok(())
}
