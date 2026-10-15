//! gpn_network_info — print the GPN network identity and compare it to the
//! canonical registry (design-network-identity-fork-tolerance.md, build item 4).
//!
//! Queries the node RPC for `system_chain`, `chain_getBlockHash(0)`,
//! `state_getRuntimeVersion`, `system_version` and `system_peers`, then reports:
//!   MATCH / DIFFERENT NETWORK / OUTDATED  + a 0-peer warning.
//!
//! Usage:
//!   gpn_network_info [--endpoint ws://127.0.0.1:9944] [--json]

use gpn_agent::network_registry;

async fn rpc(endpoint: &str, method: &str, params: serde_json::Value) -> Option<serde_json::Value> {
    let http_url = endpoint
        .replace("ws://", "http://")
        .replace("wss://", "https://");
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

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut endpoint = "ws://127.0.0.1:9944".to_string();
    let mut json_out = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--endpoint" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    endpoint = v.clone();
                }
            }
            "--json" => json_out = true,
            "--help" | "-h" => {
                println!("gpn_network_info [--endpoint ws://127.0.0.1:9944] [--json]");
                return;
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }

    let reg = network_registry::load_registry();
    let system_chain = rpc(&endpoint, "system_chain", serde_json::json!([]))
        .await
        .and_then(|v| v.as_str().map(String::from));
    let genesis = rpc(&endpoint, "chain_getBlockHash", serde_json::json!([0]))
        .await
        .and_then(|v| v.as_str().map(String::from));
    let runtime = rpc(&endpoint, "state_getRuntimeVersion", serde_json::json!([])).await;
    let spec_version = runtime
        .as_ref()
        .and_then(|v| v.get("specVersion"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let system_version = rpc(&endpoint, "system_version", serde_json::json!([]))
        .await
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "?".into());
    let peers = rpc(&endpoint, "system_peers", serde_json::json!([]))
        .await
        .and_then(|v| v.as_array().map(|a| a.len()))
        .unwrap_or(0);

    let genesis_hex = genesis.clone().unwrap_or_else(|| "0x?".into());
    let st = network_registry::classify(&genesis_hex, spec_version, &system_version);

    if json_out {
        let out = serde_json::json!({
            "system_chain": system_chain,
            "chain_id": st.chain_id,
            "genesis_hash": genesis_hex,
            "spec_version": spec_version,
            "system_version": system_version,
            "peers": peers,
            "genesis_match": st.genesis_match,
            "status": st.summary,
            "registry": {
                "network": reg.network,
                "chain_id": reg.chain_id,
                "genesis_hash": reg.genesis_hash,
                "spec_version": reg.spec_version,
                "latest_version": reg.latest_version,
                "bootnodes": reg.bootnodes,
            }
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
        return;
    }

    println!("GPN network info");
    println!("  endpoint     : {endpoint}");
    println!(
        "  system_chain : {}",
        system_chain.unwrap_or_else(|| "?".into())
    );
    println!("  chain-id     : {}", st.chain_id);
    println!("  genesis      : {genesis_hex}");
    println!("  spec_version : {spec_version}");
    println!("  node version : {system_version}");
    println!("  peers        : {peers}");
    println!("  registry     : {} ({})", reg.network, reg.chain_id);
    println!("  identity     : {}", st.summary);
    if peers == 0 {
        println!("  ⚠️ No peers — this node is NOT connected to the GPN network (isolated/fork?).");
    }
}
