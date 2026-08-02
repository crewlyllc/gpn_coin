//! gpn_explorer — lightweight read-only block explorer for the GPN network.
//!
//! Phase-4 public-testnet tooling (runbook §7 "block explorer"). Serves a
//! minimal web dashboard + JSON API over the node's JSON-RPC (via the shared
//! `ChainClient` for storage reads and raw `chain_getHeader` /
//! `chain_getBlockHash` HTTP RPC for block headers). Read-only — no signer, no
//! writes — so it can be pointed at any public node safely.
//!
//! Endpoints:
//!   GET /                          → HTML dashboard (head, difficulty, feed,
//!                                     configured account balances + attestation,
//!                                     recent blocks, marketplace jobs)
//!   GET /api/chain                 → head block + difficulty + feed (JSON)
//!   GET /api/blocks?count=12       → recent blocks (number + hash), JSON
//!   GET /api/account?address=...   → balance / nonce / staked / attested / tier
//!   GET /api/jobs                  → all marketplace jobs (state, price, parties)
//!   GET /api/feed                  → profitability feed + difficulty (JSON)
//!
//! Usage:
//!   gpn_explorer [--endpoint ws://127.0.0.1:9944] [--listen 127.0.0.1:8082]
//!               [--account //Alice] [--account //Bob] ...
//!
//! `--account` (repeatable) pre-seeds the dashboard's account panel with
//! addresses to watch (dev URIs resolve via the well-known dev keys, e.g.
//! `//Alice` → 5GrwvaEF5…). The dashboard also lets you paste an arbitrary
//! ss58/hex address in the browser.

use std::collections::HashMap;
use std::sync::Arc;

use gpn_agent::chain::{AccountInfo, ChainClient};
use tiny_http::{Header, Response, Server};

/// GPN 1 GPN = 1e12 base units (12 decimals).
const GPN_UNITS: f64 = 1_000_000_000_000.0;

#[derive(Clone)]
struct Explorer {
    client: ChainClient,
    endpoint: String,
    accounts: Vec<String>,
}

/// Resolve a dev/well-known URI (`//Alice`, etc.) to its ss58 address. `//` URIs
/// are `sp_core::sr25519::Pair` dev keys; anything that already looks like an
/// ss58 address is passed through unchanged.
fn resolve_account(raw: &str) -> String {
    if raw.starts_with("//") {
        use sp_core::sr25519::Pair;
        use sp_core::Pair as _;
        if let Ok(pair) = Pair::from_string(raw, None) {
            return pair.public().to_string();
        }
    }
    raw.to_string()
}

/// Raw JSON-RPC POST to the node's HTTP endpoint (ws:// → http://). Returns the
/// `result` field or None.
async fn rpc_call(endpoint: &str, method: &str, params: serde_json::Value) -> Option<serde_json::Value> {
    let http_url = endpoint.replace("ws://", "http://").replace("wss://", "https://");
    let body = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let resp = reqwest::Client::new()
        .post(&http_url)
        .json(&body)
        .send()
        .await
        .ok()?;
    let v: serde_json::Value = resp.json().await.ok()?;
    v.get("result").cloned()
}

/// Hex string (optionally 0x-prefixed) → u64.
fn hex_u64(h: &str) -> Option<u64> {
    u64::from_str_radix(h.trim_start_matches("0x"), 16).ok()
}

/// Current best (head) block number + hash via raw RPC. Substrate's
/// `chain_getHeader` result has NO `hash` field, so the head hash is fetched
/// separately via `chain_getBlockHash([number])`.
async fn head_block(endpoint: &str) -> Option<(u64, String)> {
    let header = rpc_call(endpoint, "chain_getHeader", serde_json::json!([])).await?;
    let number = hex_u64(header.get("number")?.as_str()?)?;
    let hash = rpc_call(endpoint, "chain_getBlockHash", serde_json::json!([number]))
        .await
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "?".into());
    Some((number, hash))
}

/// Fetch the last `count` block numbers + hashes by walking back from the head.
async fn recent_blocks(endpoint: &str, count: u64) -> Vec<(u64, String)> {
    let Some((head, _)) = head_block(endpoint).await else { return vec![] };
    let mut out = Vec::new();
    let start = head.saturating_sub(count.saturating_sub(1));
    for n in (start..=head).rev() {
        let hash = rpc_call(endpoint, "chain_getBlockHash", serde_json::json!([n]))
            .await
            .and_then(|v| v.as_str().map(|s| s.to_string()));
        out.push((n, hash.unwrap_or_else(|| "?".into())));
    }
    out
}

/// Chain summary: head block + difficulty + live profitability feed +
/// the on-chain Phase-C bond-weighted median (authoritative aggregation).
async fn chain_summary(ex: &Explorer) -> serde_json::Value {
    let (head_num, head_hash) = head_block(&ex.endpoint).await.unwrap_or((0, "?".into()));
    let difficulty = ex.client.get_difficulty().await.unwrap_or(0.0);
    let token = ex.client.get_token_price_usd().await.unwrap_or(0.0);
    let gpu = ex.client.get_gpu_hour_price("").await.unwrap_or(0.0);
    let util = ex.client.get_utilization_rate("").await.unwrap_or(0.0);
    let apr = ex.client.get_staking_apr().await.unwrap_or(0.0);
    // Authoritative on-chain Phase-C bond-weighted median (gpn_effectiveFeed RPC,
    // cycle 56). `None` when no bonded operator has posted a feed yet.
    let eff = ex.client.get_effective_feed().await;
    serde_json::json!({
        "head_number": head_num,
        "head_hash": head_hash,
        "difficulty": difficulty,
        "feed": {
            "token_price_usd": token,
            "gpu_hour_price_usd": gpu,
            "utilization": util,
            "staking_apr_percent": apr,
        },
        "effective_feed": eff.map(|f| serde_json::json!({
            "token_price_usd": f.token_price_usd,
            "gpu_hour_price_usd": f.avg_gpu_hour_price_usd,
            "utilization": f.utilization,
            "staking_apr_percent": f.staking_apr_percent,
            "updated_at_block": f.updated_at_block,
        })),
    })
}

/// Full account panel for one address: balance/nonce/staked/attested/tier.
async fn account_panel(ex: &Explorer, raw: &str) -> serde_json::Value {
    let address = resolve_account(raw);
    let info: AccountInfo = ex
        .client
        .get_account_info(&address)
        .await
        .unwrap_or(AccountInfo { address: address.clone(), balance: 0.0, nonce: 0 });
    let nonce = ex.client.get_account_nonce(&address).await;
    let (staked, attested) = ex.client.get_provider_state(&address).await;
    let attest = ex.client.get_attestation_record(&address).await;
    serde_json::json!({
        "requested": raw,
        "address": address,
        "balance_gpn": info.balance,
        "nonce": nonce,
        "staked": staked,
        "attested": attested,
        "attestation": attest.map(|(gpus, gflops, tier)| {
            serde_json::json!({ "gpus": gpus, "gflops": gflops, "tier": tier })
        }),
    })
}

/// All marketplace jobs (state, price, requestor, provider, min_gflops).
async fn jobs_panel(ex: &Explorer) -> serde_json::Value {
    let next = ex.client.get_next_job_id().await;
    let mut jobs = Vec::new();
    for id in 0..next {
        let st = ex.client.get_job_state(id).await;
        let min_gflops = ex.client.get_job_min_gflops(id).await.unwrap_or(0);
        jobs.push(match st {
            Some((state, price, requestor, provider)) => serde_json::json!({
                "id": id, "state": state,
                "price_gpn": price as f64 / GPN_UNITS,
                "requestor": requestor.unwrap_or_else(|| "?".into()),
                "provider": provider.unwrap_or_else(|| "?".into()),
                "min_gflops": min_gflops,
            }),
            None => serde_json::json!({ "id": id, "state": "?", "min_gflops": min_gflops }),
        });
    }
    serde_json::json!({ "next_job_id": next, "jobs": jobs })
}

fn json_response(body: &str, status: u16) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_string(body.to_string())
        .with_status_code(status)
        .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap())
}

fn html_response(body: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_string(body.to_string())
        .with_status_code(200)
        .with_header(Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).unwrap())
}

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter_map(|kv| {
            let mut it = kv.splitn(2, '=');
            Some((it.next()?.to_string(), it.next().unwrap_or("").to_string()))
        })
        .collect()
}

fn route(ex: &Explorer, handle: &tokio::runtime::Handle, path: &str, query: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    let params = parse_query(query);

    match path {
        "/api/chain" => json_response(&handle.block_on(chain_summary(ex)).to_string(), 200),
        "/api/feed" => json_response(&handle.block_on(chain_summary(ex)).to_string(), 200),
        "/api/effective-feed" => {
            // The authoritative on-chain Phase-C bond-weighted median alone.
            let eff = handle.block_on(ex.client.get_effective_feed());
            match eff {
                Some(f) => json_response(&serde_json::json!({
                    "token_price_usd": f.token_price_usd,
                    "gpu_hour_price_usd": f.avg_gpu_hour_price_usd,
                    "utilization": f.utilization,
                    "staking_apr_percent": f.staking_apr_percent,
                    "updated_at_block": f.updated_at_block,
                }).to_string(), 200),
                None => json_response(r#"{"error":"no bonded operator has posted a feed yet"}"#, 200),
            }
        }
        "/api/blocks" => {
            let count = params.get("count").and_then(|c| c.parse::<u64>().ok()).unwrap_or(12).min(50);
            let blocks = handle.block_on(recent_blocks(&ex.endpoint, count));
            json_response(&serde_json::json!({ "blocks": blocks }).to_string(), 200)
        }
        "/api/account" => match params.get("address") {
            Some(a) => json_response(&handle.block_on(account_panel(ex, a)).to_string(), 200),
            None => json_response(r#"{"error":"missing 'address' param"}"#, 400),
        },
        "/api/jobs" => json_response(&handle.block_on(jobs_panel(ex)).to_string(), 200),
        "/" => {
            let chain = handle.block_on(chain_summary(ex));
            let blocks = handle.block_on(recent_blocks(&ex.endpoint, 12));
            let jobs = handle.block_on(jobs_panel(ex));
            let accounts: Vec<serde_json::Value> = ex
                .accounts
                .iter()
                .map(|a| handle.block_on(account_panel(ex, a)))
                .collect();
            html_response(&dashboard_html(&chain, &blocks, &jobs, &accounts))
        }
        _ => json_response(r#"{"error":"not found"}"#, 404),
    }
}

fn dashboard_html(
    chain: &serde_json::Value,
    blocks: &[(u64, String)],
    jobs: &serde_json::Value,
    accounts: &[serde_json::Value],
) -> String {
    let feed = &chain["feed"];
    let mut s = String::new();
    s.push_str(r#"<!doctype html><html><head><meta charset="utf-8">
<title>GPN Explorer</title>
<style>body{font-family:ui-monospace,Menlo,monospace;background:#0d1117;color:#e6edf3;margin:0;padding:24px}
h1{font-size:20px} h2{font-size:15px;color:#58a6ff;margin-top:28px}
table{border-collapse:collapse;width:100%;margin-top:8px;font-size:13px}
th,td{text-align:left;padding:6px 10px;border-bottom:1px solid #21262d}
th{color:#8b949e;font-weight:600} a{color:#58a6ff}
.badge{display:inline-block;padding:2px 8px;border-radius:10px;font-size:12px;margin-right:6px}
.m{background:#1f6feb33;color:#79c0ff}.c{background:#23863633;color:#7ee787}
.s{background:#8957e533;color:#d2a8ff}.k{background:#da363333;color:#ffa198}
code{background:#161b22;padding:1px 5px;border-radius:4px}
form{margin:12px 0} input{background:#161b22;border:1px solid #30363d;color:#e6edf3;padding:6px;border-radius:6px;width:420px}
button{background:#238636;border:none;color:#fff;padding:6px 14px;border-radius:6px;cursor:pointer}</style></head><body>"#);
    s.push_str("<h1>🖥️ GPN Explorer</h1>");
    s.push_str(&format!(
        "<p>Head block <b>#{}</b> <code>{}</code> &nbsp;·&nbsp; difficulty <b>{:.0}</b></p>",
        chain["head_number"].as_u64().unwrap_or(0),
        chain["head_hash"].as_str().unwrap_or("?"),
        chain["difficulty"].as_f64().unwrap_or(0.0),
    ));
    s.push_str("<h2>Profitability feed</h2><table><tr><th>Token $</th><th>GPU $/hr</th><th>Utilization</th><th>Staking APR</th></tr>");
    s.push_str(&format!(
        "<tr><td>${:.4}</td><td>${:.2}/hr</td><td>{:.0}%</td><td>{:.1}%</td></tr></table>",
        feed["token_price_usd"].as_f64().unwrap_or(0.0),
        feed["gpu_hour_price_usd"].as_f64().unwrap_or(0.0),
        feed["utilization"].as_f64().unwrap_or(0.0) * 100.0,
        feed["staking_apr_percent"].as_f64().unwrap_or(0.0),
    ));

    // On-chain Phase-C bond-weighted median (authoritative aggregation).
    s.push_str("<h2>On-chain effective feed (Phase-C bond-weighted median)</h2>");
    match chain.get("effective_feed") {
        Some(eff) if !eff.is_null() => {
            s.push_str("<table><tr><th>Token $</th><th>GPU $/hr</th><th>Utilization</th><th>Staking APR</th><th>@ block</th></tr>");
            s.push_str(&format!(
                "<tr><td>${:.4}</td><td>${:.2}/hr</td><td>{:.0}%</td><td>{:.1}%</td><td>#{}</td></tr></table>",
                eff["token_price_usd"].as_f64().unwrap_or(0.0),
                eff["gpu_hour_price_usd"].as_f64().unwrap_or(0.0),
                eff["utilization"].as_f64().unwrap_or(0.0) * 100.0,
                eff["staking_apr_percent"].as_f64().unwrap_or(0.0),
                eff["updated_at_block"].as_u64().unwrap_or(0),
            ));
        }
        _ => s.push_str("<p>no bonded operator has posted a feed yet</p>"),
    }

    // Account lookup form
    s.push_str(r#"<h2>Account</h2><form onsubmit="window.location='/api/account?address='+encodeURIComponent(document.getElementById('addr').value);return false">
<input id="addr" placeholder="ss58 or 0x-hex address…"><button>Look up (JSON)</button></form>"#);

    if !accounts.is_empty() {
        s.push_str("<table><tr><th>Account</th><th>Balance (GPN)</th><th>Staked</th><th>Attested</th><th>Tier</th><th>GFLOPS</th></tr>");
        for a in accounts {
            let att = &a["attestation"];
            let tier = att.get("tier").and_then(|t| t.as_str()).unwrap_or("—");
            let gflops = att.get("gflops").and_then(|g| g.as_u64()).unwrap_or(0);
            s.push_str(&format!(
                "<tr><td><code>{}</code></td><td>{:.2}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                a["address"].as_str().unwrap_or("?"),
                a["balance_gpn"].as_f64().unwrap_or(0.0),
                if a["staked"].as_bool().unwrap_or(false) { "✅" } else { "—" },
                if a["attested"].as_bool().unwrap_or(false) { "✅" } else { "—" },
                tier, gflops,
            ));
        }
        s.push_str("</table>");
    }

    // Recent blocks
    s.push_str("<h2>Recent blocks</h2><table><tr><th>#</th><th>Hash</th></tr>");
    for (n, h) in blocks {
        s.push_str(&format!("<tr><td>{}</td><td><code>{}</code></td></tr>", n, h));
    }
    s.push_str("</table>");

    // Jobs
    s.push_str("<h2>Marketplace jobs</h2><table><tr><th>ID</th><th>State</th><th>Price (GPN)</th><th>min GFLOPS</th><th>Requestor</th><th>Provider</th></tr>");
    let jobs_arr = jobs["jobs"].as_array().cloned().unwrap_or_default();
    if jobs_arr.is_empty() {
        s.push_str("<tr><td colspan=6>no jobs yet</td></tr>");
    }
    for j in jobs_arr {
        let state = j["state"].as_str().unwrap_or("?");
        let badge = match state {
            "Pending" => "m", "Accepted" => "c", "Running" => "k",
            "Submitted" => "c", "Completed" => "s", _ => "m",
        };
        s.push_str(&format!(
            "<tr><td>{}</td><td><span class='badge {}'>{}</span></td><td>{:.4}</td><td>{}</td><td><code>{}</code></td><td><code>{}</code></td></tr>",
            j["id"].as_u64().unwrap_or(0),
            badge, state,
            j["price_gpn"].as_f64().unwrap_or(0.0),
            j["min_gflops"].as_u64().unwrap_or(0),
            j["requestor"].as_str().unwrap_or("?"),
            j["provider"].as_str().unwrap_or("?"),
        ));
    }
    s.push_str("</table></body></html>");
    s
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut endpoint = "ws://127.0.0.1:9944".to_string();
    let mut listen = "127.0.0.1:8082".to_string();
    let mut accounts = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--endpoint" => { i += 1; if i < args.len() { endpoint = args[i].clone(); } }
            "--listen" => { i += 1; if i < args.len() { listen = args[i].clone(); } }
            "--account" => { i += 1; if i < args.len() { accounts.push(args[i].clone()); } }
            "--help" | "-h" => {
                println!("gpn_explorer [--endpoint ws://127.0.0.1:9944] [--listen 127.0.0.1:8082] [--account //Alice] ...");
                return;
            }
            _ => {}
        }
        i += 1;
    }
    if accounts.is_empty() {
        // Sensible dev defaults so the dashboard shows something out of the box.
        accounts = vec!["//Alice".into(), "//Bob".into()];
    }
    env_logger::init();

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let client = runtime.block_on(ChainClient::new(&endpoint)).expect("chain client");
    let explorer = Arc::new(Explorer { client, endpoint: endpoint.clone(), accounts });
    let server = Server::http(&listen).expect("bind listener");
    println!("gpn_explorer listening on http://{listen} (node {endpoint})");

    for request in server.incoming_requests() {
        let url = request.url().to_string();
        let (path, query) = match url.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (url.clone(), String::new()),
        };
        let ex = explorer.clone();
        let resp = route(&ex, runtime.handle(), &path, &query);
        let _ = request.respond(resp);
    }
}
