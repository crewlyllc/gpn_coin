//! gpn_fleet — fleet supervisor agent.
//!
//! Polls a "fleet directive" (a small JSON at a URL or local path) that names the
//! target network, and keeps `gpn-node` running on that network. On a directive
//! change it stops the node, purges its data dir (networks have different
//! genesis), and restarts on the new network. Also restarts the node if it
//! crashes. Lets an operator move an entire fleet with one command
//! (`gpn_fleet_set <network>`).
//!
//! Env:
//!   GPN_FLEET_DIRECTIVE   URL or path to the directive JSON  (required)
//!   GPN_NODE_BIN          path to gpn-node (default "gpn-node")
//!   GPN_NODE_DATA         node base-path (purged on switch)
//!   GPN_NODE_ARGS         extra node args, space-separated
//!   GPN_FLEET_NETWORKS    JSON: {net: {"chain": "...", "bootnodes": "..."}}
//!   GPN_FLEET_POLL_SECS   poll interval (default 60)
//!   GPN_FLEET_STATE       last-applied network file (default ~/.gpn/fleet-state.json)

use serde_json::Value;
use std::process::{Child, Command};
use std::time::Duration;

fn env_or(k: &str, d: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| d.to_string())
}

fn fetch_directive(src: &str) -> Result<Value, String> {
    let text = if src.starts_with("http://") || src.starts_with("https://") {
        reqwest::blocking::Client::new()
            .get(src)
            .timeout(Duration::from_secs(15))
            .send()
            .map_err(|e| format!("fetch {src}: {e}"))?
            .text()
            .map_err(|e| format!("read body: {e}"))?
    } else {
        std::fs::read_to_string(src).map_err(|e| format!("read {src}: {e}"))?
    };
    serde_json::from_str(&text).map_err(|e| format!("parse directive: {e}"))
}

fn net_of(cfg: &Value, net: &str) -> (String, String) {
    let n = cfg.get(net);
    let chain = n
        .and_then(|x| x.get("chain"))
        .and_then(|x| x.as_str())
        .unwrap_or(net)
        .to_string();
    let bootnodes = n
        .and_then(|x| x.get("bootnodes"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    (chain, bootnodes)
}

fn start_node(bin: &str, data: &str, chain: &str, bootnodes: &str, extra: &[String]) -> Result<Child, String> {
    let mut cmd = Command::new(bin);
    cmd.arg("--chain").arg(chain);
    if !data.is_empty() {
        cmd.arg("--base-path").arg(data);
    }
    if !bootnodes.is_empty() {
        cmd.arg("--bootnodes").arg(bootnodes);
    }
    for a in extra {
        if !a.is_empty() {
            cmd.arg(a);
        }
    }
    println!("[fleet] starting: {bin} --chain {chain} (data={data})");
    cmd.spawn().map_err(|e| format!("spawn node: {e}"))
}

fn main() {
    let directive_src = env_or("GPN_FLEET_DIRECTIVE", "");
    if directive_src.is_empty() {
        eprintln!("set GPN_FLEET_DIRECTIVE to a URL or path for the fleet directive JSON");
        std::process::exit(1);
    }
    let node_bin = env_or("GPN_NODE_BIN", "gpn-node");
    let node_data = env_or("GPN_NODE_DATA", "");
    let node_args: Vec<String> = env_or("GPN_NODE_ARGS", "")
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();
    let poll: u64 = env_or("GPN_FLEET_POLL_SECS", "60").parse().unwrap_or(60);
    let networks: Value = serde_json::from_str(&env_or(
        "GPN_FLEET_NETWORKS",
        r#"{"testnet":{"chain":"testnet","bootnodes":""},"mainnet":{"chain":"mainnet","bootnodes":""}}"#,
    ))
    .unwrap_or_else(|_| serde_json::json!({}));

    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let state_file = env_or("GPN_FLEET_STATE", &format!("{home}/.gpn/fleet-state.json"));

    println!("== gpn_fleet ==");
    println!("  directive: {directive_src}");
    println!("  node bin:  {node_bin}");
    println!("  data:      {node_data}");
    println!("  poll:      {poll}s");

    let mut child: Option<Child> = None;
    // Remember the last network actually applied (survives restarts) so we only
    // PURGE on a real network change — never on the first start.
    let mut current: Option<String> = std::fs::read_to_string(&state_file)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.get("network").and_then(|x| x.as_str()).map(|s| s.to_string()));

    loop {
        let target = match fetch_directive(&directive_src) {
            Ok(v) => v
                .get("target_network")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            Err(e) => {
                println!("[fleet] directive error: {e} — keeping current network ({current:?})");
                String::new()
            }
        };

        if !target.is_empty() && Some(&target) != current.as_ref() {
            println!("[fleet] ► target network = {target}");
            // stop the current node
            if let Some(mut c) = child.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
            // Purge ONLY on an actual network change (different genesis), never on
            // the first start — otherwise we'd wipe a valid chain on every boot.
            let is_change = current.is_some() && current.as_deref() != Some(target.as_str());
            if is_change && !node_data.is_empty() && std::path::Path::new(&node_data).exists() {
                println!("[fleet] network changed — purging {node_data}");
                if let Err(e) = std::fs::remove_dir_all(&node_data) {
                    println!("[fleet] WARN could not purge data dir: {e}");
                }
            }
            let (chain, bootnodes) = net_of(&networks, &target);
            match start_node(&node_bin, &node_data, &chain, &bootnodes, &node_args) {
                Ok(c) => child = Some(c),
                Err(e) => println!("[fleet] ERROR {e}"),
            }
            current = Some(target.clone());
            // persist last-applied network
            let _ = std::fs::write(
                &state_file,
                serde_json::json!({"network": target}).to_string(),
            );
        }

        // supervise: restart if the node exited
        if let Some(c) = child.as_mut() {
            match c.try_wait() {
                Ok(Some(status)) => {
                    println!("[fleet] node exited ({status}) — restarting");
                    let net = current.clone().unwrap_or_default();
                    let (chain, bootnodes) = net_of(&networks, &net);
                    if let Ok(c2) = start_node(&node_bin, &node_data, &chain, &bootnodes, &node_args) {
                        child = Some(c2);
                    }
                }
                Ok(None) => {}
                Err(e) => println!("[fleet] try_wait error: {e}"),
            }
        }

        std::thread::sleep(Duration::from_secs(poll));
    }
}
