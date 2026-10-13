//! gpn_node — the **single-node supervisor** (task #25).
//!
//! One command, one config, one node. `gpn_node` starts and supervises the
//! whole GPN node — the chain node/miner, the provider heartbeat, and the
//! provider loop(s) — as **separate child processes** (fault isolation kept)
//! but presents the operator with **one** entry point: one command, one
//! prefixed log stream, one aggregated status line, and one clean shutdown.
//!
//! **A GPN node is always both a miner and an AI provider.** There is no role
//! option: every start brings up the chain node/miner *and* the AI serving
//! stack together. The node mines and serves AI, auto-yielding the one GPU to
//! a job while it runs (#24: full stop mining during a job, resume after).
//!
//! ```text
//!   gpn_node                 # starts: node + heartbeat + provider (always)
//! ```
//!
//! ## Children (all always started)
//! - the chain node/miner (`gpn-node … --force-authoring …`) — log `[node]`
//! - the heartbeat (`gpn_provider_status … :8093`) — log `[heartbeat]`
//! - the provider loop (`gpn_llm_provider …`) — log `[provider]`
//!   (+ `gpn_diffusion_provider …` when `diffusion = true`) — `[diffuse]`
//!
//! ## Config (small TOML file) + env overrides
//! The file is `--config <path>`, else `$GPN_CONFIG`, else `$GPN_PREFIX/node.toml`
//! (`$GPN_PREFIX` defaults to `~/.gpn`), else `./gpn-node.toml`. Every field has
//! an env override (`GPN_CHAIN`, `GPN_SIGNER`, `GPN_ENDPOINT`, …).
//! Secrets (the assign token, the signer/seed) are read from env / the file and
//! are **never** written anywhere by this supervisor.
//!
//! ## Supervise / shutdown / status
//! - Each child runs in its own supervisor thread; its stdout/stderr is
//!   forwarded with a `[node]`/`[heartbeat]`/`[provider]`/`[diffuse]` prefix.
//! - A child that exits unexpectedly is restarted with bounded backoff
//!   (1s → 30s), and every lifecycle event is logged.
//! - On Ctrl-C / SIGTERM every child is stopped gracefully (SIGTERM → wait →
//!   SIGKILL) and the supervisor exits.
//! - Every `GPN_STATUS_SECS` (default 15) one aggregated line is printed:
//!   child up/down + the node's best block (read cheaply from its RPC).

use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ─────────────────────────────────────────────────────────────────────────────
// Config
// ─────────────────────────────────────────────────────────────────────────────

/// The optional TOML config file. All fields optional → env fills the rest.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    chain: Option<String>,
    signer: Option<String>,
    account: Option<String>,
    endpoint: Option<String>,
    base_path: Option<String>,
    rpc_port: Option<u16>,
    node_key: Option<String>,
    bootnodes: Option<Vec<String>>,
    public_addr: Option<String>,
    /// Authority short name passed as `--<author>` (e.g. `"alice"`).
    author: Option<String>,
    /// Extra raw args appended to the `gpn-node` command.
    node_args: Option<Vec<String>>,
    provider_endpoint: Option<String>,
    assign_token: Option<String>,
    ollama: Option<String>,
    listen: Option<String>,
    gflops: Option<u64>,
    capacity: Option<u64>,
    poll_ms: Option<u64>,
    diffusion: Option<bool>,
    diffusion_poll_ms: Option<u64>,
    bin_dir: Option<String>,
    /// How often the aggregated status line is printed (seconds).
    status_secs: Option<u64>,
}

#[derive(Clone)]
struct Config {
    chain: String,
    signer: String,
    account: String,
    endpoint: String,
    base_path: Option<String>,
    rpc_port: u16,
    node_key: Option<String>,
    bootnodes: Vec<String>,
    public_addr: Option<String>,
    author: Option<String>,
    node_args: Vec<String>,
    provider_endpoint: Option<String>,
    assign_token: String,
    ollama: String,
    listen: String,
    gflops: u64,
    capacity: u64,
    poll_ms: u64,
    diffusion: bool,
    diffusion_poll_ms: u64,
    bin_dir: PathBuf,
    status_secs: u64,
}

fn env_str(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|s| !s.trim().is_empty())
}
fn env_u64(k: &str) -> Option<u64> {
    env_str(k).and_then(|s| s.parse().ok())
}
fn env_u16(k: &str) -> Option<u16> {
    env_str(k).and_then(|s| s.parse().ok())
}
fn env_bool(k: &str) -> Option<bool> {
    env_str(k).map(|s| {
        matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}
/// Split a comma/space separated list (bootnodes / node args).
fn split_list(s: &str) -> Vec<String> {
    s.split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .map(|x| x.trim())
        .filter(|x| !x.is_empty())
        .map(str::to_string)
        .collect()
}

/// Expand a leading `~` to `$HOME`.
fn expand_tilde(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    p.to_string()
}

impl Config {
    fn load() -> Result<Config, String> {
        let argv: Vec<String> = std::env::args().collect();
        // --config <path>
        let cfg_path = argv
            .iter()
            .position(|a| a == "--config")
            .and_then(|i| argv.get(i + 1).cloned())
            .or_else(|| env_str("GPN_CONFIG"));

        let file = match &cfg_path {
            Some(p) => {
                let p = expand_tilde(p);
                let text = std::fs::read_to_string(&p)
                    .map_err(|e| format!("cannot read config '{p}': {e}"))?;
                toml::from_str::<FileConfig>(&text)
                    .map_err(|e| format!("invalid config '{p}': {e}"))?
            }
            None => {
                // Default locations: $GPN_PREFIX/node.toml then ./gpn-node.toml.
                let prefix =
                    expand_tilde(&env_str("GPN_PREFIX").unwrap_or_else(|| "~/.gpn".into()));
                let candidates = [format!("{prefix}/node.toml"), "gpn-node.toml".to_string()];
                match candidates.iter().find(|p| std::path::Path::new(p).exists()) {
                    Some(p) => {
                        let text = std::fs::read_to_string(p)
                            .map_err(|e| format!("cannot read config '{p}': {e}"))?;
                        toml::from_str::<FileConfig>(&text)
                            .map_err(|e| format!("invalid config '{p}': {e}"))?
                    }
                    None => FileConfig::default(),
                }
            }
        };

        // bin dir: env GPN_BIN > file > dir of this exe > $HOME/.local/bin
        let bin_dir = env_str("GPN_BIN")
            .or(file.bin_dir)
            .map(|s| PathBuf::from(expand_tilde(&s)))
            .or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(PathBuf::from))
            })
            .unwrap_or_else(|| PathBuf::from(expand_tilde("~/.local/bin")));

        let endpoint = env_str("GPN_ENDPOINT")
            .or(file.endpoint)
            .unwrap_or_else(|| "ws://127.0.0.1:9944".to_string());
        let rpc_port = env_u16("GPN_RPC_PORT")
            .or(file.rpc_port)
            .unwrap_or(9944);

        Ok(Config {
            chain: env_str("GPN_CHAIN").or(file.chain).unwrap_or_else(|| "testnet".into()),
            signer: env_str("GPN_SIGNER")
                .or(file.signer)
                .unwrap_or_else(|| "//Alice".into()),
            account: env_str("GPN_ACCOUNT").or(file.account).unwrap_or_default(),
            endpoint,
            base_path: env_str("GPN_BASE_PATH").or(file.base_path).map(|s| expand_tilde(&s)),
            rpc_port,
            node_key: env_str("GPN_NODE_KEY").or(file.node_key),
            bootnodes: env_str("GPN_BOOTNODES")
                .map(|s| split_list(&s))
                .or(file.bootnodes)
                .unwrap_or_default(),
            public_addr: env_str("GPN_PUBLIC_ADDR").or(file.public_addr),
            author: env_str("GPN_AUTHOR").or(file.author),
            node_args: env_str("GPN_NODE_ARGS")
                .map(|s| split_list(&s))
                .or(file.node_args)
                .unwrap_or_default(),
            provider_endpoint: env_str("GPN_PROVIDER_ENDPOINT").or(file.provider_endpoint),
            assign_token: env_str("GPN_ASSIGN_TOKEN")
                .or(file.assign_token)
                .unwrap_or_default(),
            ollama: env_str("GPN_OLLAMA")
                .or(file.ollama)
                .unwrap_or_else(|| "http://127.0.0.1:11434".into()),
            listen: env_str("GPN_LISTEN")
                .or(file.listen)
                .unwrap_or_else(|| "0.0.0.0:8093".into()),
            gflops: env_u64("GPN_GFLOPS").or(file.gflops).unwrap_or(0),
            capacity: env_u64("GPN_CAPACITY").or(file.capacity).unwrap_or(1),
            poll_ms: env_u64("GPN_POLL_MS").or(file.poll_ms).unwrap_or(300),
            diffusion: env_bool("GPN_DIFFUSION").or(file.diffusion).unwrap_or(false),
            diffusion_poll_ms: env_u64("GPN_DIFFUSION_POLL_MS")
                .or(file.diffusion_poll_ms)
                .unwrap_or(2000),
            bin_dir,
            status_secs: env_u64("GPN_STATUS_SECS").or(file.status_secs).unwrap_or(15),
        })
    }

    /// Resolve a child binary by name: prefer `<bin_dir>/<name>`, else `$PATH`.
    fn resolve(&self, name: &str) -> String {
        let p = self.bin_dir.join(name);
        if p.exists() {
            p.to_string_lossy().into_owned()
        } else {
            name.to_string()
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// A supervised child
// ─────────────────────────────────────────────────────────────────────────────

/// One child process to supervise: a label, a log prefix, and a builder that
/// produces a fresh `Command` (called again on every restart).
struct ChildSpec {
    label: &'static str,
    prefix: &'static str,
    argv: Vec<String>,
    envs: Vec<(String, String)>,
}

/// Live status for one labeled child, shared with the status printer.
#[derive(Default, Clone)]
struct ChildStatus {
    up: bool,
    pid: u32,
    restarts: u64,
}

type Shared = Arc<Mutex<BTreeMap<&'static str, ChildStatus>>>;

fn set_status(shared: &Shared, label: &'static str, up: bool, pid: u32) {
    let mut m = shared.lock().unwrap();
    let e = m.entry(label).or_default();
    e.up = up;
    e.pid = pid;
    if !up {
        e.restarts += 1;
    }
}

/// Send SIGTERM on Unix (taskkill on Windows) so the child can shut down cleanly.
fn send_term(pid: u32) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Forward a child's stdout/stderr, line by line, with `[prefix] ` prepended.
fn pump<R: std::io::Read + Send + 'static>(r: R, prefix: &'static str) {
    std::thread::spawn(move || {
        let reader = BufReader::new(r);
        for line in reader.lines() {
            match line {
                Ok(l) => println!("{prefix} {l}"),
                Err(_) => break,
            }
        }
    });
}

fn build_command(spec: &ChildSpec) -> Command {
    let mut cmd = Command::new(&spec.argv[0]);
    cmd.args(&spec.argv[1..]);
    for (k, v) in &spec.envs {
        cmd.env(k, v);
    }
    cmd
}

/// Supervise one child until `shutdown` is set: run it, stream its output,
/// restart on unexpected exit with bounded backoff, and stop it gracefully.
fn supervise(spec: ChildSpec, shared: Shared, shutdown: Arc<AtomicBool>) {
    let label = spec.label;
    let prefix = spec.prefix;
    let mut backoff_ms: u64 = 1000;
    const BACKOFF_MAX_MS: u64 = 30_000;

    while !shutdown.load(Ordering::SeqCst) {
        let mut cmd = build_command(&spec);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.stdin(Stdio::null());

        let mut child: Child = match cmd.spawn() {
            Ok(c) => {
                log_event(label, &format!("started (pid {})", c.id()));
                c
            }
            Err(e) => {
                log_event(label, &format!("spawn failed: {e} — retrying in {backoff_ms}ms"));
                sleep_or_shutdown(&shutdown, backoff_ms);
                backoff_ms = (backoff_ms * 2).min(BACKOFF_MAX_MS);
                continue;
            }
        };
        let pid = child.id();
        set_status(&shared, label, true, pid);

        if let Some(out) = child.stdout.take() {
            pump(out, prefix);
        }
        if let Some(err) = child.stderr.take() {
            pump(err, prefix);
        }

        // Wait for exit (or shutdown), polling ~200ms so shutdown is prompt.
        let started = Instant::now();
        let status = loop {
            if shutdown.load(Ordering::SeqCst) {
                break None;
            }
            match child.try_wait() {
                Ok(Some(st)) => break Some(st),
                Ok(None) => std::thread::sleep(Duration::from_millis(200)),
                Err(e) => {
                    log_event(label, &format!("try_wait error: {e}"));
                    break Some(std::process::ExitStatus::default());
                }
            }
        };

        if shutdown.load(Ordering::SeqCst) {
            // Clean shutdown: SIGTERM → wait up to 10s → SIGKILL.
            log_event(label, &format!("stopping (pid {pid})"));
            send_term(pid);
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) => {
                        if Instant::now() >= deadline {
                            let _ = child.kill();
                            let _ = child.wait();
                            log_event(label, "force-killed after grace period");
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(200));
                    }
                    Err(_) => break,
                }
            }
            set_status(&shared, label, false, 0);
            break;
        }

        // Unexpected exit → restart with backoff.
        let ran = started.elapsed();
        let detail = match status {
            Some(st) => st
                .code()
                .map(|c| format!("exit code {c}"))
                .unwrap_or_else(|| "terminated by signal".to_string()),
            None => "unknown".to_string(),
        };
        log_event(
            label,
            &format!("exited ({detail}) after {}s — restarting in {backoff_ms}ms", ran.as_secs()),
        );
        set_status(&shared, label, false, 0);

        // A child that had been healthy for a while → reset the backoff.
        if ran.as_secs() >= 60 {
            backoff_ms = 1000;
        }
        sleep_or_shutdown(&shutdown, backoff_ms);
        backoff_ms = (backoff_ms * 2).min(BACKOFF_MAX_MS);
    }
    log_event(label, "supervisor thread exiting");
}

fn sleep_or_shutdown(shutdown: &Arc<AtomicBool>, ms: u64) {
    let step = 100;
    let mut left = ms;
    while left > 0 && !shutdown.load(Ordering::SeqCst) {
        let s = step.min(left);
        std::thread::sleep(Duration::from_millis(s));
        left -= s;
    }
}

fn log_event(label: &str, msg: &str) {
    let ts = now_hms();
    println!("[supervisor {ts}] {label}: {msg}");
}

fn now_hms() -> String {
    // Local-ish HH:MM:SS from the system clock (UTC is fine for log lines).
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    format!("{h:02}:{m:02}:{s:02}")
}

// ─────────────────────────────────────────────────────────────────────────────
// Status
// ─────────────────────────────────────────────────────────────────────────────

/// Cheaply read the node's best block number from its own HTTP JSON-RPC.
fn best_block(rpc_port: u16) -> Option<u64> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(1200))
        .build()
        .ok()?;
    let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"chain_getHeader","params":[]});
    let resp: serde_json::Value = client
        .post(format!("http://127.0.0.1:{rpc_port}"))
        .json(&body)
        .send()
        .ok()?
        .json()
        .ok()?;
    let hex = resp.get("result")?.get("number")?.as_str()?;
    u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok()
}

fn status_line(cfg: &Config, shared: &Shared) -> String {
    let m = shared.lock().unwrap();
    let mut parts: Vec<String> = Vec::new();
    for label in ["node", "heartbeat", "provider", "diffuse"] {
        if let Some(st) = m.get(label) {
            if st.up {
                parts.push(format!("{label}=up(pid {})", st.pid));
            } else {
                parts.push(format!("{label}=down(restarts {})", st.restarts));
            }
        }
    }
    let mut line = format!("[status] node_count={}", parts.len());
    for p in &parts {
        line.push(' ');
        line.push_str(p);
    }
    match best_block(cfg.rpc_port) {
        Some(b) => line.push_str(&format!(" best=#{b}")),
        None => line.push_str(" best=?"),
    }
    line
}

// ─────────────────────────────────────────────────────────────────────────────
// main
// ─────────────────────────────────────────────────────────────────────────────

fn build_children(cfg: &Config) -> Vec<ChildSpec> {
    let mut specs = Vec::new();

    // A GPN node is always both: mine + serve. Every node starts the chain
    // node/miner, the provider heartbeat, and the provider loop(s).
    {
        let mut argv = vec![cfg.resolve("gpn-node"), "--chain".into(), cfg.chain.clone()];
        argv.push("--force-authoring".into());
        argv.push("--rpc-port".into());
        argv.push(cfg.rpc_port.to_string());
        if let Some(a) = &cfg.author {
            argv.push(format!("--{a}"));
        }
        if let Some(k) = &cfg.node_key {
            argv.push("--node-key".into());
            argv.push(k.clone());
        }
        if let Some(bp) = &cfg.base_path {
            argv.push("--base-path".into());
            argv.push(bp.clone());
        }
        for bn in &cfg.bootnodes {
            argv.push("--bootnodes".into());
            argv.push(bn.clone());
        }
        if let Some(pa) = &cfg.public_addr {
            argv.push("--public-addr".into());
            argv.push(pa.clone());
        }
        argv.extend(cfg.node_args.iter().cloned());

        let mut envs = Vec::new();
        if !cfg.account.is_empty() {
            envs.push(("GPN_PAYOUT_ACCOUNT".to_string(), cfg.account.clone()));
        }
        envs.push(("GPN_SIGNER".to_string(), cfg.signer.clone()));

        specs.push(ChildSpec {
            label: "node",
            prefix: "[node]",
            argv,
            envs,
        });
    }

    {
        // Heartbeat (:8093): advertises capability + load to the scheduler.
        let mut argv = vec![
            cfg.resolve("gpn_provider_status"),
            "--listen".into(),
            cfg.listen.clone(),
            "--gflops".into(),
            cfg.gflops.to_string(),
            "--capacity".into(),
            cfg.capacity.to_string(),
            "--ollama".into(),
            cfg.ollama.clone(),
        ];
        if !cfg.account.is_empty() {
            argv.push("--address".into());
            argv.push(cfg.account.clone());
        }
        let mut envs = Vec::new();
        if !cfg.assign_token.is_empty() {
            envs.push(("GPN_ASSIGN_TOKEN".to_string(), cfg.assign_token.clone()));
        }
        specs.push(ChildSpec {
            label: "heartbeat",
            prefix: "[heartbeat]",
            argv,
            envs,
        });

        // Provider loop: claims + runs + settles assigned jobs.
        let mut penvs = vec![
            ("OLLAMA_URL".to_string(), cfg.ollama.clone()),
            ("SIGNER_URI".to_string(), cfg.signer.clone()),
        ];
        if let Some(pe) = &cfg.provider_endpoint {
            penvs.push(("GPN_PROVIDER_ENDPOINT".to_string(), pe.clone()));
        }
        specs.push(ChildSpec {
            label: "provider",
            prefix: "[provider]",
            argv: vec![
                cfg.resolve("gpn_llm_provider"),
                cfg.endpoint.clone(),
                cfg.signer.clone(),
                cfg.poll_ms.to_string(),
            ],
            envs: penvs,
        });

        if cfg.diffusion {
            specs.push(ChildSpec {
                label: "diffuse",
                prefix: "[diffuse]",
                argv: vec![
                    cfg.resolve("gpn_diffusion_provider"),
                    cfg.endpoint.clone(),
                    cfg.signer.clone(),
                    cfg.diffusion_poll_ms.to_string(),
                ],
                envs: vec![("SIGNER_URI".to_string(), cfg.signer.clone())],
            });
        }
    }

    specs
}

#[tokio::main]
async fn main() {
    env_logger::init();
    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("gpn_node: {e}");
            std::process::exit(2);
        }
    };

    println!("gpn_node — single-node supervisor (a node is always miner + AI provider)");
    println!(
        "  chain={} endpoint={} rpc_port={} bin_dir={}",
        cfg.chain,
        cfg.endpoint,
        cfg.rpc_port,
        cfg.bin_dir.display()
    );
    if cfg.account.is_empty() {
        println!("  account=<unset> (payout address not advertised)");
    } else {
        println!("  account={}", cfg.account);
    }
    println!("  heartbeat listen={} ollama={} gflops={}", cfg.listen, cfg.ollama, cfg.gflops);

    let specs = build_children(&cfg);
    if specs.is_empty() {
        eprintln!("gpn_node: no children to run");
        std::process::exit(2);
    }
    let labels: Vec<&'static str> = specs.iter().map(|s| s.label).collect();
    println!("  children: {}", labels.join(", "));

    let shared: Shared = Arc::new(Mutex::new(BTreeMap::new()));
    let shutdown = Arc::new(AtomicBool::new(false));

    // One supervisor thread per child.
    let mut handles = Vec::new();
    for spec in specs {
        let shared = shared.clone();
        let shutdown = shutdown.clone();
        handles.push(std::thread::spawn(move || supervise(spec, shared, shutdown)));
    }

    // Status printer.
    let status_shared = shared.clone();
    let status_shutdown = shutdown.clone();
    let status_cfg = cfg.clone();
    let status_handle = std::thread::spawn(move || {
        let interval = Duration::from_secs(status_cfg.status_secs.max(1));
        while !status_shutdown.load(Ordering::SeqCst) {
            // Sleep in small steps so shutdown is prompt.
            let mut left = interval;
            while left > Duration::ZERO && !status_shutdown.load(Ordering::SeqCst) {
                let s = Duration::from_millis(200).min(left);
                std::thread::sleep(s);
                left = left.saturating_sub(s);
            }
            if status_shutdown.load(Ordering::SeqCst) {
                break;
            }
            println!("{}", status_line(&status_cfg, &status_shared));
        }
    });

    // Wait for Ctrl-C or SIGTERM.
    wait_for_signal().await;
    println!("\n[supervisor] shutdown requested — stopping children…");
    shutdown.store(true, Ordering::SeqCst);

    for h in handles {
        let _ = h.join();
    }
    let _ = status_handle.join();
    println!("[supervisor] all children stopped — exiting");
}

#[cfg(unix)]
async fn wait_for_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    let mut intr = signal(SignalKind::interrupt()).expect("install SIGINT handler");
    tokio::select! {
        _ = term.recv() => {}
        _ = intr.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}

#[cfg(not(unix))]
async fn wait_for_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
