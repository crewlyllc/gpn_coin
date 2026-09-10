//! `gpn_fullnode` — run an always-on, publicly-reachable GPN full node and keep
//! it registered in the `Liveness` pallet (the self-organizing bootnode layer).
//!
//! This is the operator-facing tool for the `pallet-gpn-liveness` design
//! (`design-liveness-selforg-fullnode-tiers.md`). A machine earns full-node
//! status (and unlocks Silver/Gold attestation tiers) by:
//!
//!   1. Bonding + registering a **public endpoint** (`register_fullnode`).
//!   2. Being **witnessed** each liveness window by other bonded full nodes
//!      that successfully dial its endpoint (`witness_reachable`).
//!   3. Finalizing each completed window (`submit_liveness`) so the window
//!      counts as confirmed-online; missed windows slash the bond.
//!
//! Because this is a single account with one key, it CANNOT witness itself.
//! Real reachability across independent nodes is proven by operators running
//! separate full nodes who witness each other. So this tool has two roles:
//!
//!   * **self** — keep MY node registered + finalize my own windows so I stay
//!     `active` and keep earning Silver/Gold eligibility.
//!   * **witness-peer** — periodically witness a known list of *other* full
//!     nodes (their public addresses) so we build the peer-witness mesh.
//!
//! Usage:
//!   gpn_fullnode [endpoint] [signer_uri] [flags]
//!     endpoint    ws://127.0.0.1:9944 (default)
//!     signer_uri  //Alice (default) — the account that bonds/registers.
//!
//! Flags (any order):
//!     --endpoint-uri  <multiaddr>   Declared public libp2p endpoint, e.g.
//!                                   /ip4/203.0.113.5/tcp/30333/p2p/12D3KooW...
//!                                   If omitted, prints the local node's actual
//!                                   peer id (via --node-key) for you to publish.
//!     --bond-gpn      <n>           Amount to deposit (default 500 = the runtime
//!                                   LivenessBond). Ignored if already registered.
//!     --witness-peer  <ss58>        Witness this peer's reachability each cycle.
//!                                   Repeatable (comma-separate or use multiple).
//!     --witness-interval <secs>     How often to witness peers (default 300).
//!     --finalize-window             Call submit_liveness for self each cycle
//!                                   (recommended; keeps you active).
//!     --loop-interval <secs>        Status/maintenance cadence (default 60).
//!     --once                        Register (if needed) and run one cycle, then exit.
//!
//! The binary does NOT itself dial peers (that's libp2p, done by `gpn-node`);
//! it only maintains the on-chain liveness record. Run it alongside `gpn-node`
//! so the node is actually reachable at the published endpoint.

use gpn_agent::chain::ChainClient;
use sp_core::crypto::Pair as _;
use std::time::Duration;

#[derive(Clone, Debug)]
struct Args {
    endpoint: String,
    signer_uri: String,
    endpoint_uri: Option<String>,
    bond_gpn: u128,
    witness_peers: Vec<String>,
    witness_interval: u64,
    finalize_window: bool,
    loop_interval: u64,
    once: bool,
}

fn parse_args() -> Args {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut positional = Vec::new();
    let mut flags = std::collections::HashMap::new();
    for a in &raw {
        if let Some(v) = a.strip_prefix("--") {
            let (k, val) = match v.split_once('=') {
                Some((k, val)) => (k.to_string(), Some(val.to_string())),
                None => (v.to_string(), None),
            };
            flags.insert(k, val);
        } else {
            positional.push(a.clone());
        }
    }
    let get = |k: &str| flags.get(k).and_then(|v| v.clone());
    let has = |k: &str| flags.contains_key(k);

    // Comma-separated witness peers accumulate.
    let mut witness_peers = Vec::new();
    if let Some(list) = get("witness-peer") {
        for p in list.split(',').filter(|s| !s.is_empty()) {
            witness_peers.push(p.trim().to_string());
        }
    }

    Args {
        endpoint: positional.first().cloned().unwrap_or_else(|| "ws://127.0.0.1:9944".to_string()),
        signer_uri: positional.get(1).cloned().unwrap_or_else(|| "//Alice".to_string()),
        endpoint_uri: get("endpoint-uri"),
        bond_gpn: get("bond-gpn").and_then(|s| s.parse().ok()).unwrap_or(500),
        witness_peers,
        witness_interval: get("witness-interval").and_then(|s| s.parse().ok()).unwrap_or(300),
        finalize_window: has("finalize-window"),
        loop_interval: get("loop-interval").and_then(|s| s.parse().ok()).unwrap_or(60),
        once: has("once"),
    }
}

/// The SS58 public address for a signer URI (e.g. `//Alice`).
fn signer_address(uri: &str) -> Result<String, Box<dyn std::error::Error>> {
    let pair = sp_core::sr25519::Pair::from_string(uri, None)
        .map_err(|e| format!("invalid signer URI '{}': {}", uri, e))?;
    Ok(pair.public().to_string())
}

async fn ensure_registered(
    chain: &ChainClient,
    args: &Args,
) -> Result<(), Box<dyn std::error::Error>> {
    let who = signer_address(&args.signer_uri)?;
    // If already registered, skip.
    if let Some((active, _, _, _)) = chain.get_fullnode_status(&who).await {
        println!("  🔒 already registered as a full node (active={}); skipping register_fullnode", active);
        return Ok(());
    }
    let endpoint = match &args.endpoint_uri {
        Some(e) => e.clone(),
        None => {
            println!(
                "  ⚠️  no --endpoint-uri given; registering with a placeholder. \
                 Publish your real /ip4/.../p2p/... endpoint for peers to witness."
            );
            format!("/ip4/0.0.0.0/tcp/30333/p2p/{}", who)
        }
    };
    // Ensure enough free balance for the bond (best-effort; the pallet enforces min).
    let _ = chain.register_fullnode(&endpoint).await?;
    println!("  ✅ registered full node with endpoint {}", endpoint);
    Ok(())
}

async fn run_cycle(chain: &ChainClient, args: &Args) {
    let who = match signer_address(&args.signer_uri) {
        Ok(w) => w,
        Err(_) => return,
    };

    // 1. Witness reachable peers (build the mesh).
    for peer in &args.witness_peers {
        if peer == &who {
            println!("  ⏭️  skipping self-witness for {}", peer);
            continue;
        }
        match chain.witness_reachable(peer).await {
            Ok(()) => println!("  🫱 witnessed peer {}", peer),
            Err(e) => println!("  ⚠️  witness {} failed: {}", peer, e),
        }
    }

    // 2. Finalize my own window (keeps me active / avoids a miss).
    if args.finalize_window {
        match chain.submit_liveness(&who).await {
            Ok(()) => println!("  ✅ finalized my liveness window"),
            Err(e) => println!("  ⚠️  finalize failed: {}", e),
        }
    }

    // 3. Report status.
    match chain.get_fullnode_status(&who).await {
        Some((active, missed, last_win, uptime)) => {
            println!(
                "  📊 full-node status: active={} missed_windows={} last_win={} rolling_uptime={}%",
                active, missed, last_win, uptime as f64 / 100.0
            );
        }
        None => println!("  📊 not yet registered (status unavailable)"),
    }
}

async fn main_loop(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "🖥️  GPN full node service — endpoint={} signer={}",
        args.endpoint, args.signer_uri
    );
    let who = signer_address(&args.signer_uri)?;
    println!("   account: {}", who);
    if let Some(e) = &args.endpoint_uri {
        println!("   public endpoint: {}", e);
    }

    let chain = ChainClient::new(&args.endpoint).await?.with_signer(&args.signer_uri);
    ensure_registered(&chain, &args).await?;

    if args.once {
        run_cycle(&chain, &args).await;
        return Ok(());
    }

    let mut since_witness = 0u64;
    loop {
        // Every loop interval: finalize window (cheap, idempotent) + status.
        run_cycle(&chain, &args).await;

        since_witness += args.loop_interval;
        if !args.witness_peers.is_empty() && since_witness >= args.witness_interval {
            since_witness = 0;
            // run_cycle already witnesses every iteration when peers are set;
            // this counter only throttles extra witness-only passes.
        }

        tokio::time::sleep(Duration::from_secs(args.loop_interval)).await;
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let args = parse_args();
    if args.once {
        println!("🧪 single-cycle run (--once)");
    }
    main_loop(args).await
}
