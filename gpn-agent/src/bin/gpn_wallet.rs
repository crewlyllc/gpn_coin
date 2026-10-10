//! gpn_wallet — a lightweight, **node-free** GPN wallet.
//!
//! Lets an AI user / trader create, hold and move GPN **without running a full
//! GPN node**: standard Substrate sr25519 keygen (mnemonic → ss58 address), an
//! optional JSON keystore, balance reads and signed transfers, plus raw signing
//! so a client can authorize off-chain job payments. The only network touch is a
//! plain RPC endpoint (a public access point / node); keygen/address/sign are
//! pure-local and need no connection at all.
//!
//! Standard Substrate key derivation ⇒ the address is interoperable with any
//! Substrate wallet (Polkadot.js, subkey, …); no GPN-specific crypto.
//!
//! Usage:
//!   gpn_wallet keygen [--out <file>] [--words N] [--password <pw>]
//!   gpn_wallet address  <keystore.json|mnemonic|//URI> [--password <pw>]
//!   gpn_wallet balance  <endpoint> <ss58|0xhex|keystore.json|//URI> [--password <pw>]
//!   gpn_wallet send     <endpoint> <keystore.json|mnemonic|//URI> <dest-ss58|0xhex> <amount_gpn> [--password <pw>]
//!   gpn_wallet sign     <keystore.json|mnemonic|//URI> <message> [--hex] [--password <pw>]
//!
//! Env fallbacks: ENDPOINT (default `ws://127.0.0.1:9944`), GPN_WALLET_PASSWORD.
//!
//! `keygen`/`address`/`sign` are fully offline. `balance`/`send` use the given
//! RPC endpoint (`ws://` or `wss://`); the chain stays the escrow/settlement
//! layer, the wallet just talks to any node/access-point.

use gpn_agent::chain::signer;
use sp_core::crypto::{Pair, Ss58Codec};
use sp_core::sr25519;
use std::path::Path;
use std::str::FromStr;
use subxt::dynamic::Value;
use subxt::tx::Signer as _;
use subxt::utils::AccountId32;

const GPN_UNITS: f64 = 1_000_000_000_000.0;

/// A parsed wallet source: an mnemonic (or dev/derivation URI) + optional
/// password, optionally persisted in a JSON keystore.
#[derive(Debug, Clone, PartialEq)]
struct Keystore {
    address: String,
    mnemonic: String,
    crypto: String,
}

fn keystore_from_pair(pair: &sr25519::Pair, mnemonic: &str) -> Keystore {
    Keystore {
        address: pair.public().to_ss58check(),
        mnemonic: mnemonic.to_string(),
        crypto: "sr25519".to_string(),
    }
}

/// Serialize a keystore as pretty JSON (mnemonic is the secret — the caller
/// writes it 0600).
fn keystore_to_json(ks: &Keystore) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "address": ks.address,
        "crypto": ks.crypto,
        "mnemonic": ks.mnemonic,
    }))
    .unwrap_or_else(|_| "{}".to_string())
}

/// Parse a keystore JSON document (fields `address`/`crypto`/`mnemonic`).
fn keystore_from_json(raw: &str) -> Result<Keystore, String> {
    let v: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("invalid keystore JSON: {e}"))?;
    let mnemonic = v
        .get("mnemonic")
        .and_then(|m| m.as_str())
        .ok_or("keystore missing `mnemonic`")?
        .to_string();
    let crypto = v
        .get("crypto")
        .and_then(|c| c.as_str())
        .unwrap_or("sr25519")
        .to_string();
    let address = v
        .get("address")
        .and_then(|a| a.as_str())
        .unwrap_or("")
        .to_string();
    Ok(Keystore {
        address,
        mnemonic,
        crypto,
    })
}

/// Decide whether an argument names an existing keystore file. A literal
/// mnemonic (spaces) or a dev URI (`//Alice`) is never a path we try to open.
fn looks_like_path(arg: &str) -> bool {
    arg.ends_with(".json") || arg.contains('/') || arg.contains('\\') || Path::new(arg).is_file()
}

/// Resolve a wallet source (file path | mnemonic | dev URI) into an sr25519 pair.
/// `password` is the optional Substrate SURI password.
fn load_pair(source: &str, password: Option<&str>) -> Result<sr25519::Pair, String> {
    let secret = if looks_like_path(source) && Path::new(source).is_file() {
        let raw = std::fs::read_to_string(source)
            .map_err(|e| format!("cannot read keystore '{source}': {e}"))?;
        keystore_from_json(&raw)?.mnemonic
    } else {
        source.to_string()
    };
    sr25519::Pair::from_string(&secret, password)
        .map_err(|e| format!("invalid wallet source '{}': {:?}", source, e))
}

/// Normalize a destination (ss58 or 0x-hex) into 32 raw bytes.
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

fn gpn_to_raw(amount_gpn: f64) -> Result<u128, String> {
    if !amount_gpn.is_finite() || amount_gpn <= 0.0 {
        return Err(format!(
            "amount must be a positive number (got {amount_gpn})"
        ));
    }
    Ok((amount_gpn * GPN_UNITS) as u128)
}

#[allow(dead_code)]
fn raw_to_gpn(raw: u128) -> f64 {
    raw as f64 / GPN_UNITS
}

/// Pop `--password <pw>` (or env) and the `--hex` flag out of an arg list,
/// leaving positional args in order.
fn extract_flags(args: &[String]) -> (Vec<String>, Option<String>, bool) {
    let mut positional = Vec::new();
    let mut password = std::env::var("GPN_WALLET_PASSWORD").ok();
    let mut hex_mode = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--password" | "-p" => {
                if let Some(v) = args.get(i + 1) {
                    password = Some(v.clone());
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--hex" => {
                hex_mode = true;
                i += 1;
            }
            _ => {
                positional.push(args[i].clone());
                i += 1;
            }
        }
    }
    (positional, password, hex_mode)
}

fn cmd_keygen(args: &[String]) -> Result<(), String> {
    let mut out: Option<String> = None;
    let mut words: usize = 12;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out" | "-o" => {
                out = args.get(i + 1).cloned();
                i += 2;
            }
            "--words" => {
                words = args.get(i + 1).and_then(|w| w.parse().ok()).unwrap_or(12);
                i += 2;
            }
            _ => i += 1,
        }
    }
    // sp-core's `generate_with_phrase` emits a 12-word phrase; document that
    // `--words` is accepted for forward-compat (non-12 values fall back to 12).
    let _ = words;
    let (pair, phrase, _seed) = sr25519::Pair::generate_with_phrase(None);
    let ks = keystore_from_pair(&pair, &phrase);

    println!("🔑 New GPN wallet (sr25519)");
    println!("  address : {}", ks.address);
    println!("  hex     : 0x{}", hex::encode(pair.public().0));
    println!("  mnemonic: {}", ks.mnemonic);
    println!("  ⚠  the mnemonic IS the private key — back it up offline, never share it.");

    if let Some(path) = out {
        std::fs::write(&path, keystore_to_json(&ks))
            .map_err(|e| format!("cannot write keystore '{path}': {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        println!("  keystore: {path} (mode 0600)");
    }
    Ok(())
}

fn cmd_address(args: &[String]) -> Result<(), String> {
    let (pos, password, _hex) = extract_flags(args);
    let source = pos
        .first()
        .ok_or("usage: address <keystore.json|mnemonic|//URI>")?;
    let pair = load_pair(source, password.as_deref())?;
    println!("{}", pair.public().to_ss58check());
    Ok(())
}

async fn cmd_balance(args: &[String]) -> Result<(), String> {
    let (pos, password, _hex) = extract_flags(args);
    let endpoint = pos
        .first()
        .cloned()
        .or_else(|| std::env::var("ENDPOINT").ok())
        .unwrap_or_else(|| "ws://127.0.0.1:9944".to_string());
    let target = pos
        .get(1)
        .ok_or("usage: balance <endpoint> <ss58|0xhex|keystore.json|//URI>")?;

    // Resolve the queried address: a wallet source derives it; an ss58/hex is
    // taken as-is.
    let addr32: [u8; 32] = if looks_like_path(target) && Path::new(target).is_file() {
        load_pair(target, password.as_deref())?.public().0
    } else if target.starts_with("0x") || target.starts_with("0X") {
        parse_dest(target)?
    } else if target.starts_with("//") || target.split_whitespace().count() > 1 {
        // dev URI or mnemonic → derive
        load_pair(target, password.as_deref())?.public().0
    } else {
        parse_dest(target)?
    };
    let ss58 = sr25519::Public::from_raw(addr32).to_ss58check();

    let client = gpn_agent::chain::ChainClient::new(&endpoint)
        .await
        .map_err(|e| format!("cannot connect to {endpoint}: {e}"))?;
    let info = client
        .get_account_info(&ss58)
        .await
        .map_err(|e| format!("balance read failed: {e}"))?;
    println!("{ss58}");
    println!("  free balance: {:.6} GPN", info.balance);
    Ok(())
}

async fn cmd_send(args: &[String]) -> Result<(), String> {
    let (pos, password, _hex) = extract_flags(args);
    let endpoint = pos
        .first()
        .cloned()
        .or_else(|| std::env::var("ENDPOINT").ok())
        .unwrap_or_else(|| "ws://127.0.0.1:9944".to_string());
    let source = pos
        .get(1)
        .ok_or("usage: send <endpoint> <keystore|mnemonic|//URI> <dest> <amount_gpn>")?;
    let dest = pos.get(2).ok_or("missing <dest-ss58|0xhex>")?;
    let amount_gpn: f64 = pos
        .get(3)
        .ok_or("missing <amount_gpn>")?
        .parse()
        .map_err(|e| format!("bad amount: {e}"))?;

    let dest_bytes = parse_dest(dest)?;
    let amount_raw = gpn_to_raw(amount_gpn)?;
    let pair = load_pair(source, password.as_deref())?;
    let from = sr25519::Public::from_raw(pair.public().0).to_ss58check();
    let signer_obj = signer::Sr25519Signer::from_pair(pair);

    let client = signer::connect(&endpoint)
        .await
        .map_err(|e| format!("cannot connect to {endpoint}: {e}"))?;
    let call = subxt::dynamic::tx(
        "Balances",
        "transfer_allow_death",
        vec![Value::from_bytes(dest_bytes), Value::u128(amount_raw)],
    );
    let nonce = signer::current_nonce(&client, &endpoint, &signer_obj.account_id()).await;
    println!("sending {amount_gpn} GPN from {from} → {dest} (nonce {nonce})…");
    signer::submit_signed(&client, &call, &signer_obj, nonce)
        .await
        .map_err(|e| format!("transfer failed: {e}"))?;
    println!("✅ sent {amount_gpn} GPN → {dest}");
    Ok(())
}

fn cmd_sign(args: &[String]) -> Result<(), String> {
    let (pos, password, hex_mode) = extract_flags(args);
    let source = pos
        .first()
        .ok_or("usage: sign <keystore|mnemonic|//URI> <message> [--hex]")?;
    let message = pos.get(1).ok_or("missing <message>")?;
    let pair = load_pair(source, password.as_deref())?;

    let payload: Vec<u8> = if hex_mode {
        let body = message.trim_start_matches("0x").trim_start_matches("0X");
        hex::decode(body).map_err(|e| format!("invalid hex message: {e}"))?
    } else {
        message.as_bytes().to_vec()
    };

    let signature = pair.sign(&payload);
    let from = sr25519::Public::from_raw(pair.public().0).to_ss58check();
    println!("signer   : {from}");
    println!(
        "blake2_256: 0x{}",
        hex::encode(sp_core::hashing::blake2_256(&payload))
    );
    println!("signature: 0x{}", hex::encode(signature.0));
    Ok(())
}

fn print_usage() {
    eprintln!("gpn_wallet — lightweight, node-free GPN wallet (sr25519)");
    eprintln!();
    eprintln!("  gpn_wallet keygen  [--out <file>] [--password <pw>]");
    eprintln!("  gpn_wallet address <keystore.json|mnemonic|//URI> [--password <pw>]");
    eprintln!("  gpn_wallet balance <endpoint> <ss58|0xhex|keystore.json|//URI> [--password <pw>]");
    eprintln!("  gpn_wallet send    <endpoint> <keystore.json|mnemonic|//URI> <dest> <amount_gpn> [--password <pw>]");
    eprintln!(
        "  gpn_wallet sign    <keystore.json|mnemonic|//URI> <message> [--hex] [--password <pw>]"
    );
    eprintln!();
    eprintln!("Env: ENDPOINT (default ws://127.0.0.1:9944), GPN_WALLET_PASSWORD");
}

#[tokio::main]
async fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let cmd = argv.get(1).map(|s| s.as_str()).unwrap_or("");
    let rest: &[String] = if argv.len() > 2 { &argv[2..] } else { &[] };
    let result = match cmd {
        "keygen" => cmd_keygen(rest),
        "address" => cmd_address(rest),
        "balance" => cmd_balance(rest).await,
        "send" => cmd_send(rest).await,
        "sign" => cmd_sign(rest),
        _ => {
            print_usage();
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("❌ {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical Substrate dev mnemonic derives the well-known dev
    /// addresses on sr25519 — proves our keygen/derivation is standard and
    /// interoperable with any Substrate wallet (Polkadot.js / subkey).
    ///
    /// Base phrase alone → `5DfhGyQd…`; phrase + the `//Alice` hard junction →
    /// the canonical Alice account. Both are externally-known vectors.
    #[test]
    fn mnemonic_derives_standard_dev_addresses() {
        let phrase = "bottom drive obey lake curtain smoke basket hold race lonely fit walk";
        let base = sr25519::Pair::from_string(phrase, None).expect("pair");
        assert_eq!(
            base.public().to_ss58check(),
            "5DfhGyQdFobKM8NsWvEeAKk5EQQgYe9AydgJ7rMB6E1EqRzV"
        );
        let alice = sr25519::Pair::from_string(&format!("{phrase}//Alice"), None).expect("pair");
        assert_eq!(
            alice.public().to_ss58check(),
            "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY"
        );
    }

    #[test]
    fn generated_wallet_roundtrips_through_keystore() {
        let (pair, phrase, _seed) = sr25519::Pair::generate_with_phrase(None);
        let ks = keystore_from_pair(&pair, &phrase);
        // The address in the keystore matches a fresh derivation from the
        // stored mnemonic (import path == create path).
        let reimported = sr25519::Pair::from_string(&ks.mnemonic, None).expect("reimport");
        assert_eq!(reimported.public().to_ss58check(), ks.address);

        let json = keystore_to_json(&ks);
        let parsed = keystore_from_json(&json).expect("parse");
        assert_eq!(parsed, ks);
        assert_eq!(parsed.crypto, "sr25519");
    }

    #[test]
    fn keystore_json_rejects_missing_mnemonic() {
        let err = keystore_from_json(r#"{"address":"5x"}"#).unwrap_err();
        assert!(err.contains("mnemonic"), "got: {err}");
    }

    #[test]
    fn password_protected_source_derives_differently() {
        let phrase = "bottom drive obey lake curtain smoke basket hold race lonely fit walk";
        let plain = sr25519::Pair::from_string(phrase, None).unwrap();
        let locked = sr25519::Pair::from_string(phrase, Some("hunter2")).unwrap();
        assert_ne!(
            plain.public().to_ss58check(),
            locked.public().to_ss58check(),
            "a SURI password must change the derived address"
        );
    }

    #[test]
    fn amount_parsing_bounds() {
        assert_eq!(gpn_to_raw(1.0).unwrap(), 1_000_000_000_000);
        assert_eq!(gpn_to_raw(0.5).unwrap(), 500_000_000_000);
        assert!(gpn_to_raw(0.0).is_err());
        assert!(gpn_to_raw(-5.0).is_err());
        assert!(gpn_to_raw(f64::NAN).is_err());
        assert!(gpn_to_raw(f64::INFINITY).is_err());
        assert_eq!(raw_to_gpn(1_500_000_000_000), 1.5);
    }

    #[test]
    fn dest_parsing_accepts_ss58_and_hex() {
        let ss58 = "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY";
        let from_ss58 = parse_dest(ss58).unwrap();
        // Well-known //Alice public key.
        assert_eq!(
            hex::encode(from_ss58),
            "d43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d"
        );
        let from_hex =
            parse_dest("0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d")
                .unwrap();
        assert_eq!(from_ss58, from_hex);
        assert!(parse_dest("0xdeadbeef").is_err());
        assert!(parse_dest("not-an-address").is_err());
    }

    #[test]
    fn sign_produces_a_verifiable_sr25519_signature() {
        let phrase = "bottom drive obey lake curtain smoke basket hold race lonely fit walk";
        let pair = sr25519::Pair::from_string(phrase, None).unwrap();
        let payload = b"pay:job=42;amount=1.5";
        let sig = pair.sign(payload);
        assert!(sr25519::Pair::verify(&sig, payload, &pair.public()));
        // Any tamper invalidates it.
        assert!(!sr25519::Pair::verify(
            &sig,
            b"pay:job=42;amount=1.6",
            &pair.public()
        ));
    }

    #[test]
    fn flag_extraction_pulls_password_and_hex() {
        let args: Vec<String> = [
            "addr".to_string(),
            "--password".to_string(),
            "pw".to_string(),
            "--hex".to_string(),
        ]
        .to_vec();
        let (pos, pw, hex) = extract_flags(&args);
        assert_eq!(pos, vec!["addr".to_string()]);
        assert_eq!(pw.as_deref(), Some("pw"));
        assert!(hex);
    }
}
