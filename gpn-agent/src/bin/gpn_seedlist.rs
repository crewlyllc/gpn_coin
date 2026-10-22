//! `gpn_seedlist` — publish `seeds.json` from on-chain liveness.
//!
//! This is the unprivileged exporter from
//! `design-network-membership-bootstrap.md` §3.4 ("auto-verification and
//! promotion"). It reads `Liveness::FullNodes` / `Endpoints` / `WitnessCounts`,
//! applies the published promotion rule (distinct witnesses + rolling uptime +
//! freshness + a dialable multiaddr), ranks the survivors and writes the
//! `seeds.json` document that nodes fetch via `--bootnodes-url`.
//!
//! Why it matters: the dynamic peer set can then change **without shipping a
//! client**. Because every input is on-chain, anyone can re-run this and get the
//! same list — a captured publisher cannot inject a node that peers would
//! accept (the libp2p handshake is the real gate; `peer_id` is in the address).
//!
//! Source A (the ≤5 hardcoded seeds in `network.json`) is deliberately *outside*
//! this pipeline: it has to keep working when the pipeline itself is broken, so
//! it is never derived from here.
//!
//! Usage:
//!   gpn_seedlist [endpoint] [--out seeds.json] [--ttl 300] [--max 16]
//!                [--min-witnesses 3] [--min-uptime-bps 9500]
//!                [--network <name>] [--publisher <name>] [--print]
//!
//! Exit codes: 0 published (even if the list is empty — "nobody qualifies" is a
//! valid, honest publication), 1 chain unreachable / write failed.

use gpn_agent::chain::ChainClient;
use gpn_agent::seedlist;
use std::time::{SystemTime, UNIX_EPOCH};

/// `0x…` AccountId32 → canonical ss58 (what the rest of the tooling shows).
fn hex_to_ss58(hex_addr: &str) -> Option<String> {
    use sp_core::crypto::Ss58Codec;
    let raw = hex_addr.strip_prefix("0x").unwrap_or(hex_addr);
    let bytes: [u8; 32] = hex::decode(raw).ok()?.try_into().ok()?;
    Some(sp_core::sr25519::Public::from_raw(bytes).to_ss58check())
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let idx = args.iter().position(|a| a == name)?;
    args.get(idx + 1).cloned()
}

fn has(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn num_or<T: std::str::FromStr>(args: &[String], name: &str, default: T) -> T {
    flag(args, name)
        .and_then(|v| v.parse::<T>().ok())
        .unwrap_or(default)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Positional endpoint = the first non-flag argument that is not the value of
    // a known flag.
    let mut endpoint = "ws://127.0.0.1:9944".to_string();
    let mut skip_next = false;
    for a in &args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if a.starts_with("--") {
            skip_next = true;
            continue;
        }
        endpoint = a.clone();
        break;
    }

    let out_path = flag(&args, "--out").unwrap_or_else(|| "seeds.json".to_string());
    let ttl = num_or(&args, "--ttl", seedlist::DEFAULT_TTL);
    let max = num_or(&args, "--max", seedlist::MAX_SEEDS);
    let min_witnesses = num_or(&args, "--min-witnesses", seedlist::MIN_WITNESSES);
    let min_uptime = num_or(&args, "--min-uptime-bps", seedlist::MIN_UPTIME_BPS);
    let publisher = flag(&args, "--publisher").unwrap_or_else(|| "gpn_seedlist".to_string());
    let print_only = has(&args, "--print");

    println!("gpn_seedlist → {} (chain {})", out_path, endpoint);

    let chain = match ChainClient::new(&endpoint).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("❌ could not connect to {endpoint}: {e}");
            std::process::exit(1);
        }
    };
    let nodes = chain.list_liveness_nodes().await;
    if nodes.is_empty() {
        // An unreachable chain and a chain with no full nodes both land here.
        // Distinguish them so a publisher's CI fails loudly on the former.
        if chain.head_block_number().await.is_none() {
            eprintln!("❌ chain unreachable at {endpoint} — refusing to publish a blank list");
            std::process::exit(1);
        }
    }

    let window = chain.current_liveness_window().await;
    let entries: Vec<seedlist::SeedEntry> =
        seedlist::rank(&nodes, window, min_witnesses, min_uptime, max)
            // Publish addresses in ss58, the form every other GPN tool shows.
            .into_iter()
            .map(|mut e| {
                if let Some(ss58) = hex_to_ss58(&e.account) {
                    e.account = ss58;
                }
                e
            })
            .collect();

    // The registry the binary was built for — never re-derived from a URL, so a
    // republished list cannot silently retarget a different network.
    let reg = gpn_agent::network_registry::load_registry();

    println!(
        "  window {window} · {} registered full node(s) · {} candidate(s) published",
        nodes.len(),
        entries.len()
    );
    for n in &nodes {
        let why = if seedlist::is_candidate(n, window, min_witnesses, min_uptime) {
            "✅"
        } else if !n.active {
            "✗ inactive"
        } else if n.reachable < min_witnesses {
            "✗ too few witnesses"
        } else if n.uptime_bps < min_uptime {
            "✗ uptime"
        } else if n.last_confirmed_window.saturating_add(1) < window {
            "✗ stale"
        } else {
            "✗ no dialable endpoint"
        };
        println!(
            "   {why:>22}  {}  uptime={}bps winners={} last_window={}  {}",
            hex_to_ss58(&n.account).unwrap_or_else(|| n.account.clone()),
            n.uptime_bps,
            n.reachable,
            n.last_confirmed_window,
            if n.endpoint.is_empty() {
                "-"
            } else {
                &n.endpoint
            }
        );
    }

    let doc = seedlist::seeds_json(
        &reg.network,
        &reg.chain_id,
        &reg.genesis_hash,
        now_unix(),
        ttl,
        &publisher,
        window,
        &entries,
    );
    let body = serde_json::to_string_pretty(&doc)?;

    if print_only {
        println!("{body}");
        return Ok(());
    }

    // Atomic publish: a reader polling the URL must never see a half-written
    // file (CDN/rsync hosts would otherwise serve a truncated JSON).
    let tmp = format!("{out_path}.tmp");
    std::fs::write(&tmp, format!("{body}\n"))?;
    std::fs::rename(&tmp, &out_path)?;
    println!(
        "  wrote {out_path} ({} multiaddrs, ttl {ttl}s)",
        entries.len()
    );
    Ok(())
}
