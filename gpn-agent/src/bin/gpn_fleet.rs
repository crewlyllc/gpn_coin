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
use std::time::{Duration, Instant};

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

    // The node child, shared with the self-heal watchdog (it must be able to
    // stop the node before moving a DB, and the loop must not respawn it into
    // the middle of that move).
    let child_slot: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
    let hold = Arc::new(AtomicBool::new(false));

    // Self-heal (#157): ON by default, every node, no launcher line required.
    // See `gpn_agent::selfheal` and the same watchdog in `gpn_node`.
    spawn_self_heal(
        &node_data,
        rpc_port,
        child_slot.clone(),
        hold.clone(),
        node_up.clone(),
    );

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
        // A self-heal is swapping the chain DB: do not restart the node.
        if hold.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }

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
            if let Some(mut c) = take_child(&child_slot) {
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
            child_slot.lock().unwrap().is_none()
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
                    *child_slot.lock().unwrap() = Some(c);
                }
                Err(e) => println!("[fleet] ERROR {e}"),
            }
        }

        // supervise: restart if the node exited
        let exited = {
            let mut slot = child_slot.lock().unwrap();
            match slot.as_mut() {
                Some(c) => match c.try_wait() {
                    Ok(Some(status)) => Some(Ok(status)),
                    Ok(None) => None,
                    Err(e) => Some(Err(e.to_string())),
                },
                None => None,
            }
        };
        if let Some(res) = exited {
            match res {
                Ok(status) => println!("[fleet] node exited ({status}) — restarting"),
                Err(e) => println!("[fleet] try_wait error: {e}"),
            }
            node_up.store(false, Ordering::SeqCst);
            *child_slot.lock().unwrap() = None;
        }

        std::thread::sleep(Duration::from_secs(poll));
    }
}

/// Take the child out of the shared slot (leaving `None`).
fn take_child(slot: &Arc<Mutex<Option<Child>>>) -> Option<Child> {
    slot.lock().unwrap().take()
}

/// The fleet supervisor's half of a self-heal: stop the node we own, and let the
/// main loop start a fresh one when the repair is done.
struct FleetNode {
    slot: Arc<Mutex<Option<Child>>>,
    hold: Arc<AtomicBool>,
}

impl gpn_agent::selfheal::NodeControl for FleetNode {
    fn stop(&self) -> Result<(), String> {
        // Hold first: the main loop respawns on exit, and a respawn during the
        // rename would reopen the DB we are moving.
        self.hold.store(true, Ordering::SeqCst);
        let child = take_child(&self.slot);
        let Some(mut c) = child else {
            return Ok(());
        };
        let pid = c.id();
        let _ = c.kill();
        // Escalate: a node that ignores the kill must not let us rename a live DB.
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            match c.try_wait() {
                Ok(Some(_)) => return Ok(()),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(250));
                }
                Ok(None) => {
                    self.hold.store(false, Ordering::SeqCst);
                    return Err(format!(
                        "node child (pid {pid}) did not stop within 60s — refusing to move a live DB"
                    ));
                }
                Err(e) => {
                    self.hold.store(false, Ordering::SeqCst);
                    return Err(format!("try_wait (pid {pid}): {e}"));
                }
            }
        }
    }
    fn start(&self) -> Result<(), String> {
        // The main loop starts the node on its next pass (≤ one poll interval).
        self.hold.store(false, Ordering::SeqCst);
        Ok(())
    }
}

/// Run the divergence self-heal for a fleet-supervised node.
fn spawn_self_heal(
    base: &str,
    rpc_port: u16,
    slot: Arc<Mutex<Option<Child>>>,
    hold: Arc<AtomicBool>,
    node_up: Arc<AtomicBool>,
) {
    let enabled = !matches!(
        std::env::var("GPN_SELF_HEAL")
            .ok()
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref(),
        Some("0") | Some("false") | Some("off") | Some("no")
    );
    if !enabled {
        println!("[fleet] self-heal disabled (GPN_SELF_HEAL=0)");
        return;
    }
    let base = if base.trim().is_empty() {
        {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            std::path::PathBuf::from(home).join(".gpn/node")
        }
    } else {
        std::path::PathBuf::from(base)
    };
    let Some(snap) = snapshot_bin() else {
        println!(
            "[fleet] self-heal DISABLED: gpn_snapshot is not next to this binary, so a fork              could not be repaired (set GPN_SNAPSHOT_BIN)"
        );
        return;
    };
    let meta_src = std::env::var("GPN_SNAPSHOT_META_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| gpn_agent::selfheal::DEFAULT_SNAPSHOT_META_URL.to_string());
    let interval = std::env::var("GPN_SELF_HEAL_CHECK_SECS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(gpn_agent::selfheal::CHECK_INTERVAL_SECS)
        .max(60);
    let start_delay = std::env::var("GPN_SELF_HEAL_START_SECS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(120);

    let wc = gpn_agent::selfheal::WatchConfig {
        check: gpn_agent::selfheal::CheckConfig {
            rpc: format!("http://127.0.0.1:{rpc_port}"),
            base: base.clone(),
            meta_src,
            pubkey_hex: gpn_agent::update::RELEASE_PUBKEY_HEX.to_string(),
            ledger: None,
        },
        snapshot_dir: base.join("snapshots"),
        interval_secs: interval,
        start_delay_secs: start_delay,
    };
    let control = FleetNode {
        slot: slot.clone(),
        hold: hold.clone(),
    };
    let import = move |snapshot: &std::path::Path, b: &std::path::Path| {
        let mut cmd = std::process::Command::new(&snap);
        cmd.arg("import")
            .arg("--snapshot")
            .arg(snapshot)
            .arg("--base-path")
            .arg(b);
        let out = cmd
            .output()
            .map_err(|e| format!("spawn {}: {e}", snap.display()))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "{} import exited {}: {}",
                snap.display(),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    };
    let up = move || node_up.load(Ordering::SeqCst);
    let busy = || gpn_agent::selfheal::job_in_flight();
    let stop = Arc::new(AtomicBool::new(false));
    let sleep = |secs: u64| std::thread::sleep(Duration::from_secs(secs));
    let log = |line: &str| {
        let line = line.strip_prefix("self-heal: ").unwrap_or(line);
        println!("[fleet] self-heal: {line}");
    };
    std::thread::spawn(move || {
        gpn_agent::selfheal::watch(wc, &control, &import, &up, &busy, &stop, &sleep, &log);
    });
}

/// `gpn_snapshot` next to this executable (the release ships them together).
fn snapshot_bin() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var("GPN_SNAPSHOT_BIN")
        .ok()
        .filter(|v| !v.is_empty())
    {
        return Some(std::path::PathBuf::from(p));
    }
    let mut p = std::env::current_exe().ok()?;
    p.set_file_name(if cfg!(windows) {
        "gpn_snapshot.exe"
    } else {
        "gpn_snapshot"
    });
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}
