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
//!   GPN_GUI               1 = serve the local dashboard (same as --gui)
//!   GPN_GUI_LISTEN        dashboard bind address (default 127.0.0.1:8099)
//!   GPN_IDLE_FORCE        1 = bypass the sole-authority refusal for idle
//!
//! The dashboard is the same loopback-only panel `gpn_node --gui` serves: idle
//! mode is a file (`~/.gpn/idle.json`) read by the node's miner and every
//! provider, so it works identically under this supervisor.

use serde_json::Value;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn env_or(k: &str, d: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| d.to_string())
}

fn env_flag(k: &str) -> bool {
    matches!(
        env_or(k, "").trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// `--name value` or `--name=value`.
fn arg_value(args: &[String], name: &str) -> Option<String> {
    let eq = format!("{name}=");
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
        .or_else(|| {
            args.iter()
                .find_map(|a| a.strip_prefix(&eq).map(str::to_string))
        })
}

/// Dashboard enablement from argv + env (env is equivalent, argv wins for the
/// listen address).
fn gui_config(argv: &[String]) -> (bool, Option<String>, bool) {
    let enabled = argv.iter().any(|a| a == "--gui") || env_flag("GPN_GUI");
    let listen = std::env::var("GPN_GUI_LISTEN")
        .ok()
        .or_else(|| arg_value(argv, "--gui-listen"));
    let force_idle = argv.iter().any(|a| a == "--force-idle") || env_flag("GPN_IDLE_FORCE");
    (enabled, listen, force_idle)
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

fn start_node(
    bin: &str,
    data: &str,
    chain: &str,
    bootnodes: &str,
    extra: &[String],
) -> Result<Child, String> {
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

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let (gui_enabled, gui_listen, force_idle) = gui_config(&argv);
    let rpc_port: u16 = arg_value(&node_args, "--rpc-port")
        .and_then(|p| p.parse().ok())
        .unwrap_or(9944);

    println!("== gpn_fleet ==");
    println!("  directive: {directive_src}");
    println!("  node bin:  {node_bin}");
    println!("  data:      {node_data}");
    println!("  poll:      {poll}s");

    // Supervisor-owned state the dashboard cannot probe for itself.
    let node_up = Arc::new(AtomicBool::new(false));
    let chain_name = Arc::new(Mutex::new(String::new()));
    // Held for the process lifetime: dropping a GuiHandle stops the accept loop.
    let mut _dashboard: Option<gpn_agent::gui::GuiHandle> = None;

    if gui_enabled {
        let up = node_up.clone();
        let chain = chain_name.clone();
        let ollama = env_or("GPN_OLLAMA_URL", "http://127.0.0.1:11434");
        let account = env_or("GPN_GUI_ACCOUNT", &env_or("GPN_MINER_ACCOUNT", ""));
        let source: Arc<dyn gpn_agent::gui::StatusSource> = Arc::new(move || {
            let is_up = up.load(Ordering::SeqCst);
            let net = chain.lock().map(|c| c.clone()).unwrap_or_default();
            let children = vec![serde_json::json!({
                "label": "node",
                "up": is_up,
                "pid": serde_json::Value::Null,
                "restarts": 0,
                "detail": if is_up { String::new() } else { "not running".to_string() },
            })];
            gpn_agent::nodestatus::status_doc(&gpn_agent::nodestatus::StatusParams {
                node_up: is_up,
                chain: &net,
                rpc_port,
                account: &account,
                standby: env_flag("GPN_STANDBY"),
                children,
                ollama: &ollama,
            })
        });
        let mut opts = gpn_agent::gui::GuiOptions::default();
        if let Some(listen) = gui_listen {
            opts.listen = listen;
        }
        opts.force_idle = force_idle;
        // Auto-idle (#94): same sampler as gpn_node — opt-in, dashboard-toggleable.
        let auto_cfg = gpn_agent::idle_auto::AutoIdleConfig::from_env();
        let auto_shared = Arc::new(gpn_agent::idle_auto::AutoIdleShared::new(
            auto_cfg.clone(),
            gpn_agent::idle_auto::initial_enabled(),
            gpn_agent::idle_auto::toggle_path(),
        ));
        {
            let peers_port = rpc_port;
            gpn_agent::idle_auto::spawn(gpn_agent::idle_auto::RunOptions {
                idle_path: gpn_agent::idle::idle_path(),
                force: force_idle,
                peers: Arc::new(move || gpn_agent::nodestatus::health(peers_port).map(|(p, _)| p)),
                probe: Box::new(gpn_agent::idle_auto::NvidiaProbe::new(0)),
                tick_secs: auto_cfg.busy_sample_secs,
                stop: Arc::new(AtomicBool::new(false)),
                shared: auto_shared.clone(),
            });
        }
        opts.auto_idle = Some(auto_shared);
        // Keep the handle ALIVE for the process lifetime: dropping it stops the
        // accept loop (the supervisor owns the dashboard thread).
        match gpn_agent::gui::spawn(opts, source) {
            Ok(handle) => {
                println!(
                    "  dashboard: {} (loopback only — open it in a browser)",
                    handle.url()
                );
                _dashboard = Some(handle);
            }
            Err(e) => println!("[fleet] dashboard not started: {e}"),
        }
    } else {
        println!("{}", gpn_agent::gui::startup_hint("gpn_fleet"));
    }

    let mut child: Option<Child> = None;
    // Remember the last network actually applied (survives restarts) so we only
    // PURGE on a real network change — never on the first start.
    let mut current: Option<String> = std::fs::read_to_string(&state_file)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| {
            v.get("network")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
        });

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

        // Decide whether to (re)start the node this cycle.
        let start_needed = if !target.is_empty() && Some(&target) != current.as_ref() {
            println!("[fleet] ► target network = {target}");
            // stop the current node
            if let Some(mut c) = child.take() {
                node_up.store(false, Ordering::SeqCst);
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
            current = Some(target.clone());
            let _ = std::fs::write(
                &state_file,
                serde_json::json!({"network": target}).to_string(),
            );
            true
        } else {
            // No directive change — (re)start only if the node isn't running.
            child.is_none()
        };

        if start_needed {
            let net = current.clone().unwrap_or_default();
            if let Ok(mut c) = chain_name.lock() {
                *c = net.clone();
            }
            let (chain, bootnodes) = net_of(&networks, &net);
            match start_node(&node_bin, &node_data, &chain, &bootnodes, &node_args) {
                Ok(c) => {
                    node_up.store(true, Ordering::SeqCst);
                    child = Some(c);
                }
                Err(e) => println!("[fleet] ERROR {e}"),
            }
        }

        // supervise: restart if the node exited
        if let Some(c) = child.as_mut() {
            match c.try_wait() {
                Ok(Some(status)) => {
                    println!("[fleet] node exited ({status}) — restarting");
                    node_up.store(false, Ordering::SeqCst);
                    let net = current.clone().unwrap_or_default();
                    let (chain, bootnodes) = net_of(&networks, &net);
                    if let Ok(c2) =
                        start_node(&node_bin, &node_data, &chain, &bootnodes, &node_args)
                    {
                        node_up.store(true, Ordering::SeqCst);
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
