//! gpn_faucet — off-chain drip faucet for the GPN public testnet.
//!
//! Runbook §5 option 3 (recommended): a small HTTP service backed by a funded
//! testnet key that drips a fixed amount of GPN to a requesting address via a
//! rate-limited signed `Balances.transfer`. **Zero runtime change** — keeps the
//! fair-launch mainnet runtime pristine and can be shut off instantly.
//!
//! Why off-chain (vs a mint extrinsic): the testnet spec has NO endowed accounts
//! (fair-launch economics preserved). The faucet key (default `//Alice`, the
//! testnet ops/sudo key) starts at 0 and must first MINE GPN via GPU PoW (run
//! `gpn-node --chain testnet --alice --force-authoring` or an external GPU
//! miner) to accumulate a drip balance. The faucet enforces a reserve so it can
//! never drain itself below `--reserve-gpn`.
//!
//! Rate limiting (per address, in-memory — fine for a small testnet):
//!   - `--cooldown-secs`  : minimum seconds between drips to the same address
//!   - `--daily-cap-gpn`  : max GPN to one address per UTC day
//!   - `--total-cap-gpn`  : max GPN to one address ever
//!   - `--reserve-gpn`    : faucet stops dripping if its balance would fall below this
//!
//! Endpoints:
//!   GET /drip?address=<ss58|hex>&amount=<gpn>   → drip GPN (fixed `--drip-gpn`; a
//!                                                 caller may request a SMALLER
//!                                                 drip, never a larger one)
//!   GET /status                                  → faucet balance + config + rate-limit state
//!   GET /                                        → tiny usage banner
//!
//! The address may be an ss58 account (e.g. 5GrwvaEF…, parsed via
//! `AccountId32::from_str`) or raw 0x-prefixed hex (64 chars).
//!
//! Usage:
//!   gpn_faucet [--endpoint ws://127.0.0.1:9944] [--listen 127.0.0.1:8081]
//!             [--signer-uri //Alice] [--drip-gpn 10000] [--cooldown-secs 3600]
//!             [--daily-cap-gpn 100000] [--total-cap-gpn 1000000]
//!             [--reserve-gpn 1000]
//!
//! Uses the shared `signer::Sr25519Signer` + `signer::submit_signed` +
//! `signer::current_nonce` plumbing (Cycle-39 dispatch-outcome verified), so a
//! drip that is *included but fails dispatch* (e.g. InsufficientBalance) is
//! reported as an error, not a false success.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use gpn_agent::chain::signer;
use subxt::dynamic::Value;
use subxt::tx::Signer as _;
use subxt::utils::AccountId32;
use tiny_http::{Header, Response, Server};

/// 1 GPN = 1e12 base units (12 decimals).
const GPN_UNITS: f64 = 1_000_000_000_000.0;

/// Per-address drip ledger (rate limiting state).
#[derive(Default)]
struct DripLedger {
    /// address -> (last_drip_unix_secs, day_index, total_dripped_today, total_dripped_ever)
    entries: HashMap<String, (u64, u64, f64, f64)>,
}

#[derive(Clone)]
struct Faucet {
    endpoint: String,
    signer_uri: String,
    drip_gpn: f64,
    cooldown_secs: u64,
    daily_cap_gpn: f64,
    total_cap_gpn: f64,
    reserve_gpn: f64,
    ledger: Arc<Mutex<DripLedger>>,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// UTC day index (unix_secs / 86400) — used to roll the daily cap at midnight UTC.
fn day_index(secs: u64) -> u64 {
    secs / 86_400
}

/// Parse a faucet target address: accepts ss58 (via AccountId32::FromStr) or
/// 0x-prefixed hex (64 chars). Returns the raw 32 account bytes.
fn parse_address(raw: &str) -> Result<[u8; 32], String> {
    let t = raw.trim();
    if t.starts_with("0x") || t.starts_with("0X") {
        let hex_body = &t[2..];
        if hex_body.len() != 64 {
            return Err(format!(
                "hex address must be 64 chars (got {})",
                hex_body.len()
            ));
        }
        let bytes = hex::decode(hex_body).map_err(|e| format!("invalid hex: {}", e))?;
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        return Ok(out);
    }
    // ss58 form
    let acct =
        AccountId32::from_str(t).map_err(|e| format!("invalid ss58 address '{}': {}", t, e))?;
    Ok(acct.0)
}

/// The `Balances.transfer_allow_death(dest, value)` call (this Substrate
/// version renamed `Balances::transfer` → `transfer_allow_death`; see the
/// metadata probe). The GPN runtime declares `type Address = AccountId` (bare
/// `AccountId32`), so `dest` encodes as the raw account bytes — matching the
/// pattern proven in `gpn_distribute_rewards` (it passes account params to
/// `Balances.force_set_balance` as plain `Value::from_bytes`).
fn transfer_call(dest_bytes: &[u8; 32], amount_raw: u128) -> impl subxt::tx::Payload {
    subxt::dynamic::tx(
        "Balances",
        "transfer_allow_death",
        vec![Value::from_bytes(dest_bytes), Value::u128(amount_raw)],
    )
}

/// Resolve the amount to drip. The faucet drips a **fixed** `drip_gpn`; a caller
/// may *lower* the requested amount (e.g. to take a smaller drip) but must never
/// exceed `drip_gpn` — otherwise one request could drain the whole daily cap.
/// A missing, non-positive, or non-finite `amount` falls back to `drip_gpn`.
fn resolve_drip_amount(drip_gpn: f64, requested: Option<f64>) -> f64 {
    match requested {
        Some(a) if a.is_finite() && a > 0.0 => a.min(drip_gpn),
        _ => drip_gpn,
    }
}

/// Check rate limits and, if allowed, record the drip atomically.
/// Returns Ok(()) if the drip is permitted, Err(msg) with the reason otherwise.
fn check_and_record(faucet: &Faucet, addr_key: &str, amount_gpn: f64) -> Result<(), String> {
    let now = now_secs();
    let today = day_index(now);
    let mut ledger = faucet.ledger.lock().unwrap();
    let e = ledger
        .entries
        .entry(addr_key.to_string())
        .or_insert((0, 0, 0.0, 0.0));

    // Roll the daily counter if a new UTC day started.
    if e.1 != today {
        e.1 = today;
        e.2 = 0.0;
    }

    if e.0 != 0 && now.saturating_sub(e.0) < faucet.cooldown_secs {
        return Err(format!(
            "rate-limited: wait {}s more (cooldown {}s)",
            faucet.cooldown_secs - now.saturating_sub(e.0),
            faucet.cooldown_secs
        ));
    }
    if e.2 + amount_gpn > faucet.daily_cap_gpn {
        return Err(format!(
            "daily cap exceeded: {:.2}/{:.2} GPN today",
            e.2, faucet.daily_cap_gpn
        ));
    }
    if e.3 + amount_gpn > faucet.total_cap_gpn {
        return Err(format!(
            "total cap exceeded: {:.2}/{:.2} GPN ever",
            e.3, faucet.total_cap_gpn
        ));
    }

    e.0 = now;
    e.2 += amount_gpn;
    e.3 += amount_gpn;
    Ok(())
}

/// Read the faucet key's on-chain balance (GPN). 0 if unreachable/empty.
async fn faucet_balance(faucet: &Faucet) -> f64 {
    let Ok(signer) = signer::Sr25519Signer::from_uri(&faucet.signer_uri) else {
        return 0.0;
    };
    let Ok(client) = signer::connect(&faucet.endpoint).await else {
        return 0.0;
    };
    let acct = signer.account_id();
    let addr = subxt::dynamic::storage(
        "System",
        "Account",
        vec![Value::from_bytes(acct.as_ref() as &[u8; 32])],
    );
    let http_url = faucet
        .endpoint
        .replace("ws://", "http://")
        .replace("wss://", "https://");
    let body = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "chain_getBlockHash", "params": [] });
    // Head block (not finalized — PoW dev/testnet chains have no finality gadget).
    let head: Option<subxt::utils::H256> = {
        async {
            let resp = reqwest::Client::new()
                .post(&http_url)
                .json(&body)
                .send()
                .await
                .ok()?;
            let v: serde_json::Value = resp.json().await.ok()?;
            let hex_body = v.get("result")?.as_str()?.trim_start_matches("0x");
            let bytes = hex::decode(hex_body).ok()?;
            if bytes.len() != 32 {
                return None;
            }
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            Some(arr.into())
        }
        .await
    };
    let fetch = async {
        let thunk = match head {
            Some(h) => client.storage().at(h).fetch(&addr).await.ok()?,
            None => client
                .storage()
                .at_latest()
                .await
                .ok()?
                .fetch(&addr)
                .await
                .ok()?,
        }?;
        let v = thunk.to_value().ok()?.remove_context();
        use subxt::ext::scale_value::ValueDef;
        if let ValueDef::Composite(c) = &v.value {
            if let subxt::ext::scale_value::Composite::Named(pairs) = c {
                if let Some((_, data)) = pairs.iter().find(|(k, _)| k == "data") {
                    if let ValueDef::Composite(subxt::ext::scale_value::Composite::Named(fields)) =
                        &data.value
                    {
                        if let Some((_, free)) = fields.iter().find(|(k, _)| k == "free") {
                            return free.as_u128().map(|x| x as f64 / GPN_UNITS);
                        }
                    }
                }
            }
        }
        None
    };
    fetch.await.unwrap_or(0.0)
}

/// Perform a single drip: rate-limit check, then signed `Balances.transfer`.
async fn drip(faucet: &Faucet, dest_bytes: &[u8; 32], amount_gpn: f64) -> Result<String, String> {
    let amount_raw: u128 = (amount_gpn * GPN_UNITS) as u128;

    let faucet_bal = faucet_balance(faucet).await;
    if faucet_bal < faucet.reserve_gpn {
        return Err(format!(
            "faucet under reserve: balance {:.2} GPN < reserve {:.2} GPN (mine GPN into the faucet key first)",
            faucet_bal, faucet.reserve_gpn
        ));
    }
    if faucet_bal - amount_gpn < faucet.reserve_gpn {
        return Err(format!(
            "drip would breach reserve: {:.2} - {:.2} < {:.2} GPN reserve",
            faucet_bal, amount_gpn, faucet.reserve_gpn
        ));
    }

    let signer = signer::Sr25519Signer::from_uri(&faucet.signer_uri)
        .map_err(|e| format!("invalid signer URI: {}", e))?;
    let client = signer::connect(&faucet.endpoint)
        .await
        .map_err(|e| format!("cannot connect to {}: {}", faucet.endpoint, e))?;
    let call = transfer_call(dest_bytes, amount_raw);
    let nonce = signer::current_nonce(&client, &faucet.endpoint, &signer.account_id()).await;
    signer::submit_signed(&client, &call, &signer, nonce)
        .await
        .map_err(|e| format!("transfer failed dispatch: {}", e))?;

    Ok(format!(
        "dripped {:.4} GPN to {} (faucet balance before: {:.2} GPN)",
        amount_gpn,
        AccountId32(*dest_bytes),
        faucet_bal
    ))
}

fn json_response(body: &str, status: u16) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_string(body.to_string())
        .with_status_code(status)
        .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap())
}

fn route(
    faucet: &Faucet,
    handle: &tokio::runtime::Handle,
    path: &str,
    query: &str,
) -> Response<std::io::Cursor<Vec<u8>>> {
    // /status
    if path == "/status" || path == "/" && query.contains("status") {
        let bal = handle.block_on(faucet_balance(faucet));
        let ledger = faucet.ledger.lock().unwrap();
        let mut entries: Vec<_> = ledger.entries.iter().collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        let body = serde_json::json!({
            "faucet": {
                "endpoint": faucet.endpoint,
                "signer": faucet.signer_uri,
                "drip_gpn": faucet.drip_gpn,
                "cooldown_secs": faucet.cooldown_secs,
                "daily_cap_gpn": faucet.daily_cap_gpn,
                "total_cap_gpn": faucet.total_cap_gpn,
                "reserve_gpn": faucet.reserve_gpn,
            },
            "balance_gpn": bal,
            "drip_count": entries.len(),
            "recent": entries.iter().rev().take(10).map(|(a, (t, _d, today, ever))| {
                serde_json::json!({ "address": a, "last_drip_unix": t, "today_gpn": today, "ever_gpn": ever })
            }).collect::<Vec<_>>(),
        });
        return json_response(&body.to_string(), 200);
    }

    // /drip?address=...&amount=...
    if path == "/drip" {
        let params: HashMap<String, String> = query
            .split('&')
            .filter_map(|kv| {
                let mut it = kv.splitn(2, '=');
                Some((it.next()?.to_string(), it.next().unwrap_or("").to_string()))
            })
            .collect();
        let Some(raw_addr) = params.get("address") else {
            return json_response(
                r#"{"ok":false,"error":"missing 'address' param (ss58 or 0x-hex)"}"#,
                400,
            );
        };
        let dest_bytes = match parse_address(raw_addr) {
            Ok(b) => b,
            Err(e) => return json_response(&format!(r#"{{"ok":false,"error":"{}"}}"#, e), 400),
        };
        // A caller may request a SMALLER drip, but never more than the fixed
        // `--drip-gpn`: unbounded, one request could take the whole daily cap
        // (red-team finding #6).
        let amount_gpn = resolve_drip_amount(
            faucet.drip_gpn,
            params.get("amount").and_then(|a| a.parse::<f64>().ok()),
        );
        let addr_key = AccountId32(dest_bytes).to_string();

        if let Err(e) = check_and_record(faucet, &addr_key, amount_gpn) {
            return json_response(&format!(r#"{{"ok":false,"error":"{}"}}"#, e), 429);
        }
        // Rate-limit recorded BEFORE the tx: if the transfer itself fails, we
        // still count the attempt (conservative — prevents hammering a broken
        // path). Could be relaxed to only count on success.
        return match handle.block_on(drip(faucet, &dest_bytes, amount_gpn)) {
            Ok(msg) => json_response(
                &serde_json::json!({ "ok": true, "message": msg, "amount_gpn": amount_gpn })
                    .to_string(),
                200,
            ),
            Err(e) => json_response(&format!(r#"{{"ok":false,"error":"{}"}}"#, e), 500),
        };
    }

    // / (usage banner)
    let body = "GPN testnet faucet. GET /drip?address=<ss58|0xhex>&amount=<gpn>  |  GET /status";
    Response::from_string(body.to_string()).with_status_code(200)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    // Build a tokio runtime for the async chain calls, then run the blocking
    // HTTP loop on the main thread. We deliberately do NOT use
    // `#[tokio::main]` (it would make the main thread a tokio worker, and
    // `Handle::block_on` from a tokio worker panics: "Cannot start a runtime
    // from within a runtime"). A plain `fn main` + a hand-built runtime lets
    // us `block_on` the chain calls from the non-tokio HTTP loop thread.
    let rt = tokio::runtime::Runtime::new()?;
    let handle = rt.handle().clone();

    let mut endpoint = "ws://127.0.0.1:9944".to_string();
    let mut listen = "127.0.0.1:8081".to_string();
    let mut signer_uri = "//Alice".to_string();
    let mut drip_gpn = 10_000.0;
    let mut cooldown_secs = 3600u64;
    let mut daily_cap_gpn = 100_000.0;
    let mut total_cap_gpn = 1_000_000.0;
    let mut reserve_gpn = 1_000.0;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = || {
            args.next()
                .ok_or_else(|| gpn_agent::cli::ArgError::from(format!("missing value for {a}")))
        };
        match a.as_str() {
            "--endpoint" => endpoint = val()?,
            "--listen" => listen = val()?,
            "--signer-uri" => signer_uri = val()?,
            "--drip-gpn" => drip_gpn = gpn_agent::cli::parse_num("--drip-gpn", &val()?)?,
            "--cooldown-secs" => {
                cooldown_secs = gpn_agent::cli::parse_num("--cooldown-secs", &val()?)?
            }
            "--daily-cap-gpn" => {
                daily_cap_gpn = gpn_agent::cli::parse_num("--daily-cap-gpn", &val()?)?
            }
            "--total-cap-gpn" => {
                total_cap_gpn = gpn_agent::cli::parse_num("--total-cap-gpn", &val()?)?
            }
            "--reserve-gpn" => reserve_gpn = gpn_agent::cli::parse_num("--reserve-gpn", &val()?)?,
            other => {
                return Err(gpn_agent::cli::ArgError::from(format!("unknown arg: {other}")).into())
            }
        }
    }

    let faucet = Faucet {
        endpoint,
        signer_uri,
        drip_gpn,
        cooldown_secs,
        daily_cap_gpn,
        total_cap_gpn,
        reserve_gpn,
        ledger: Arc::new(Mutex::new(DripLedger::default())),
    };

    let server = Server::http(&listen).map_err(|e| format!("cannot bind {}: {}", listen, e))?;
    println!(
        "GPN faucet listening on http://{} (endpoint {})",
        listen, faucet.endpoint
    );
    println!(
        "  signer={} drip={:.0} GPN cooldown={}s daily_cap={:.0} total_cap={:.0} reserve={:.0}",
        faucet.signer_uri,
        faucet.drip_gpn,
        faucet.cooldown_secs,
        faucet.daily_cap_gpn,
        faucet.total_cap_gpn,
        faucet.reserve_gpn
    );
    println!("  GET /drip?address=<ss58|0xhex>&amount=<gpn>   GET /status");

    for request in server.incoming_requests() {
        let url = request.url().to_string();
        let (path, query) = match url.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (url.clone(), String::new()),
        };
        let resp = route(&faucet, &handle, &path, &query);
        let _ = request.respond(resp);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_address_accepts_ss58() {
        // Alice dev key
        let b = parse_address("5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY").unwrap();
        assert_eq!(b.len(), 32);
    }

    #[test]
    fn parse_address_accepts_hex() {
        let hex_s = format!("0x{}", hex::encode([0xabu8; 32]));
        let b = parse_address(&hex_s).unwrap();
        assert_eq!(b, [0xabu8; 32]);
    }

    #[test]
    fn parse_address_rejects_bad_input() {
        assert!(parse_address("nonsense").is_err());
        assert!(parse_address("0xabcd").is_err()); // too short
        assert!(parse_address(
            "0xzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"
        )
        .is_err()); // bad hex
    }

    #[test]
    fn transfer_call_builds_balances_transfer() {
        // Just ensure the call constructs without panicking (full decode
        // requires live metadata). It targets the Balances pallet's `transfer`.
        let dest = [1u8; 32];
        let _call = transfer_call(&dest, 1_000_000_000_000_000);
    }

    #[test]
    fn drip_amount_defaults_to_fixed_when_absent() {
        assert_eq!(resolve_drip_amount(10_000.0, None), 10_000.0);
    }

    #[test]
    fn drip_amount_cannot_exceed_fixed_drip() {
        // A caller asking for the whole daily cap in one shot is bounded to drip_gpn.
        assert_eq!(resolve_drip_amount(10_000.0, Some(100_000.0)), 10_000.0);
    }

    #[test]
    fn drip_amount_allows_smaller_request() {
        assert_eq!(resolve_drip_amount(10_000.0, Some(250.0)), 250.0);
    }

    #[test]
    fn drip_amount_rejects_nonpositive_and_nonfinite() {
        // Falls back to the fixed drip (never a negative/NaN/inf transfer).
        assert_eq!(resolve_drip_amount(10_000.0, Some(-5.0)), 10_000.0);
        assert_eq!(resolve_drip_amount(10_000.0, Some(0.0)), 10_000.0);
        assert_eq!(resolve_drip_amount(10_000.0, Some(f64::NAN)), 10_000.0);
        assert_eq!(resolve_drip_amount(10_000.0, Some(f64::INFINITY)), 10_000.0);
    }

    #[test]
    fn rate_limit_enforces_cooldown() {
        let faucet = Faucet {
            endpoint: "".into(),
            signer_uri: "//Alice".into(),
            drip_gpn: 100.0,
            cooldown_secs: 3600,
            daily_cap_gpn: 100_000.0,
            total_cap_gpn: 1_000_000.0,
            reserve_gpn: 1000.0,
            ledger: Arc::new(Mutex::new(DripLedger::default())),
        };
        let key = "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY".to_string();
        assert!(check_and_record(&faucet, &key, 100.0).is_ok());
        // second drip within cooldown -> rejected
        assert!(check_and_record(&faucet, &key, 100.0).is_err());
    }

    #[test]
    fn rate_limit_rolls_daily_cap() {
        let faucet = Faucet {
            endpoint: "".into(),
            signer_uri: "//Alice".into(),
            drip_gpn: 100.0,
            cooldown_secs: 0,
            daily_cap_gpn: 150.0,
            total_cap_gpn: 1_000_000.0,
            reserve_gpn: 1000.0,
            ledger: Arc::new(Mutex::new(DripLedger::default())),
        };
        let key = "addr".to_string();
        assert!(check_and_record(&faucet, &key, 100.0).is_ok());
        // 100 + 100 > 150 daily cap -> rejected
        assert!(check_and_record(&faucet, &key, 100.0).is_err());
    }

    #[test]
    fn rate_limit_enforces_total_cap() {
        let faucet = Faucet {
            endpoint: "".into(),
            signer_uri: "//Alice".into(),
            drip_gpn: 100.0,
            cooldown_secs: 0,
            daily_cap_gpn: 1_000_000.0,
            total_cap_gpn: 150.0,
            reserve_gpn: 1000.0,
            ledger: Arc::new(Mutex::new(DripLedger::default())),
        };
        let key = "addr".to_string();
        assert!(check_and_record(&faucet, &key, 100.0).is_ok());
        // 100 + 100 > 150 total cap -> rejected
        assert!(check_and_record(&faucet, &key, 100.0).is_err());
    }
}
