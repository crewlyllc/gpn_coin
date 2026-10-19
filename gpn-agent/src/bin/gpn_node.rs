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
//! **“One GPU, one job” = MUTUAL EXCLUSION (one job *at a time*), NOT a limit on
//! job types.** Every node is equipped by design to serve **every** job type —
//! LLM chat · image · video · ML inference · fine-tune — and advertises all of
//! them. A default start brings up *all* providers; per-capability OPT-OUT envs
//! exist for constrained boxes (`GPN_DIFFUSION=0`, `GPN_ML=0`,
//! `GPN_FINETUNE=0`) but the default is “everything available”.
//!
//! ```text
//!   gpn_node                 # starts: node + heartbeat + all 5 providers (default)
//! ```
//!
//! ## Children (all started by default)
//! - the chain node/miner (`gpn-node …` — mines by default) — log `[node]`
//! - the heartbeat (`gpn_provider_status … :8093`) — log `[heartbeat]`
//! - the LLM provider loop (`gpn_llm_provider …`) — log `[provider]`
//! - the image/video provider (`gpn_diffusion_provider …`) — log `[diffuse]`
//!   (opt-out: `GPN_DIFFUSION=0`)
//! - the ML-inference provider (`gpn_ml_provider …`) — log `[ml]`
//!   (opt-out: `GPN_ML=0`)
//! - the fine-tune provider (`gpn_finetune_provider …`) — log `[finetune]`
//!   (opt-out: `GPN_FINETUNE=0`)
//!
//! A missing model/dependency for one optional provider is **non-fatal**: the
//! node logs a clear warning and keeps the node + the other providers healthy;
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
//!   forwarded with a `[node]`/`[heartbeat]`/`[provider]`/`[diffuse]`/`[ml]`/
//!   `[finetune]` prefix.
//! - A child that exits unexpectedly is restarted with bounded backoff
//!   (1s → 30s), and every lifecycle event is logged.
//! - On Ctrl-C / SIGTERM every child is stopped gracefully (SIGTERM → wait →
//!   SIGKILL) and the supervisor exits.
//! - Every `GPN_STATUS_SECS` (default 15) one aggregated line is printed:
//!   child up/down + the node's best block (read cheaply from its RPC).

use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
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
    ml: Option<bool>,
    ml_poll_ms: Option<u64>,
    finetune: Option<bool>,
    finetune_poll_ms: Option<u64>,
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
    base_path: PathBuf,
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
    ml: bool,
    ml_poll_ms: u64,
    finetune: bool,
    finetune_poll_ms: u64,
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

// ─────────────────────────────────────────────────────────────────────────────
// Console encoding safety (legacy Windows consoles)
// ─────────────────────────────────────────────────────────────────────────────

/// When true, every line the supervisor prints is sanitised to ASCII (`?`/
/// tokens in place of emoji) so it survives a legacy Windows console and a
/// PowerShell `Tee-Object` pipeline that round-trips the text through the OEM
/// code page — otherwise UTF-8 emoji mojibake (`ΓÜá∩╕Å`, `≡ƒÆñ`), even with
/// `chcp 65001`. Off on Unix (modern terminals render UTF-8 fine).
static ASCII_MODE: AtomicBool = AtomicBool::new(false);

/// Turn ASCII-safe console output on/off (set once at startup).
fn set_ascii_mode(on: bool) {
    ASCII_MODE.store(on, Ordering::Relaxed);
}

/// Whether the supervisor is sanitising console output to ASCII.
fn ascii_mode() -> bool {
    ASCII_MODE.load(Ordering::Relaxed)
}

/// Decide the ASCII-console default: `--no-emoji` or `GPN_ASCII_LOG=1` force it
/// on; `GPN_ASCII_LOG=0` forces it off; otherwise it defaults **on** on Windows
/// (legacy consoles mojibake UTF-8) and **off** elsewhere.
fn ascii_mode_default(argv: &[String]) -> bool {
    if argv.iter().any(|a| a == "--no-emoji") {
        return true;
    }
    match env_str("GPN_ASCII_LOG") {
        Some(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        None => cfg!(windows),
    }
}

/// Map (or drop) a non-ASCII char to a safe ASCII token. Known symbols get a
/// readable token; variation selectors / zero-width joiners are dropped; any
/// other non-ASCII (e.g. an emoji) becomes `?`. Always returns ASCII.
fn ascii_token(ch: char) -> &'static str {
    match ch {
        '⚠' => "!",
        'ℹ' => "i",
        '✅' => "[ok]",
        '❌' | '✖' => "[x]",
        '🚀' => "[go]",
        '📦' => "[pkg]",
        '🏆' => "[win]",
        '💤' => "[zzz]",
        '🔧' => "[fix]",
        '🛑' => "[stop]",
        '⛏' => "[mine]",
        '🎁' => "[blk]",
        '🔨' => "[pow]",
        // common typography → ASCII look-alikes (keeps messages readable)
        '—' | '–' | '·' => "-",
        '…' => "...",
        '’' | '‘' => "'",
        '“' | '”' => "\"",
        '│' => "|",
        '→' => "->",
        '×' => "x",
        // invisible/formatting code points: drop them
        '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}' => "",
        c if ('\u{FE00}'..='\u{FE0F}').contains(&c) => "", // variation selectors
        _ => "?",
    }
}

/// Return `s` unchanged if it is already ASCII, else a copy with every
/// non-ASCII char replaced by a safe ASCII token (see [`ascii_token`]).
fn to_ascii_safe(s: &str) -> String {
    if s.is_ascii() {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_ascii() {
            out.push(ch);
        } else {
            out.push_str(ascii_token(ch));
        }
    }
    out
}

/// `asciiprintln!` that honours ASCII mode. Used for **all** supervisor output so a
/// legacy Windows console never shows mojibake.
macro_rules! asciiprintln {
    ($($arg:tt)*) => {{
        let line = format!($($arg)*);
        if crate::ascii_mode() {
            ::std::println!("{}", crate::to_ascii_safe(&line));
        } else {
            ::std::println!("{line}");
        }
    }};
}

/// `asciieprintln!` that honours ASCII mode.
macro_rules! asciieprintln {
    ($($arg:tt)*) => {{
        let line = format!($($arg)*);
        if crate::ascii_mode() {
            ::std::eprintln!("{}", crate::to_ascii_safe(&line));
        } else {
            ::std::eprintln!("{line}");
        }
    }};
}

// ─────────────────────────────────────────────────────────────────────────────
// Base path + DB-pruning detection
// ─────────────────────────────────────────────────────────────────────────────

/// The pinned default node base path used when neither `GPN_BASE_PATH` nor the
/// config file's `base_path` is set.
///
/// **Why pinned at all?** Substrate, with no `--base-path`, writes to an
/// OS-default data dir (`%LOCALAPPDATA%\gpn-node.exe\data` on Windows,
/// `~/.local/share/gpn-node` on Unix). A guard that only inspects an *explicit*
/// base path therefore looks at the WRONG directory when none is set — the node
/// creates/reads its DB somewhere else — and can force `archive` onto a legacy
/// DB → `Incompatible pruning modes` → crash-loop. Pinning a single path makes
/// the DB-existence check and the node's `--base-path` agree.
fn default_base_path() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(r"C:\gpn\data")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(expand_tilde("~/.gpn/node"))
    }
}

/// The node's **OS-default** data dir (what Substrate uses with no
/// `--base-path`). Used only for backward-compat detection of a DB created
/// before we pinned a base path. Windows: `%LOCALAPPDATA%\gpn-node.exe\data`.
fn os_default_node_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var("LOCALAPPDATA")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|la| PathBuf::from(la).join("gpn-node.exe").join("data"))
    }
    #[cfg(not(windows))]
    {
        std::env::var("HOME")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|h| PathBuf::from(h).join(".local/share/gpn-node"))
    }
}

/// True if any `<base>/chains/*/db` already exists (a pre-existing DB).
fn db_exists_at(base: &Path) -> bool {
    let chains = base.join("chains");
    let Ok(rd) = std::fs::read_dir(&chains) else {
        return false;
    };
    rd.flatten().any(|e| e.path().join("db").is_dir())
}

/// Marker file we drop next to a DB **we** created with archive pruning.
/// Its presence is the only thing that lets us safely request `archive` on an
/// existing DB (Substrate locks the mode at creation; a legacy/unknown DB would
/// hard-fail `Incompatible pruning modes`).
const PRUNING_MARKER: &str = ".gpn-pruning-archive";

fn has_archive_marker(base: &Path) -> bool {
    base.join(PRUNING_MARKER).is_file()
}

fn write_archive_marker(base: &Path) {
    // Best-effort: a missing marker only means the next start leaves pruning
    // alone (safe), never a crash.
    let _ = std::fs::create_dir_all(base);
    let _ = std::fs::write(base.join(PRUNING_MARKER), b"archive\n");
}

/// Resolve the effective base path the node will actually use, and keep
/// backward-compat with a legacy DB at the OS-default location.
///
/// * explicit (`GPN_BASE_PATH` / config `base_path`) wins — unless it has no DB
///   while the OS-default dir DOES (then we point at the existing DB so we
///   don't silently start a fresh chain and re-sync from scratch).
/// * otherwise the pinned [`default_base_path`], with the same OS-default
///   fallback if that legacy DB exists.
fn resolve_effective_base(explicit: Option<PathBuf>) -> PathBuf {
    resolve_effective_base_with(explicit, os_default_node_dir())
}

/// Testable core of [`resolve_effective_base`].
fn resolve_effective_base_with(explicit: Option<PathBuf>, legacy: Option<PathBuf>) -> PathBuf {
    let pinned = explicit.unwrap_or_else(default_base_path);
    if !db_exists_at(&pinned) {
        if let Some(legacy) = legacy {
            if legacy != pinned && db_exists_at(&legacy) {
                asciieprintln!(
                    "[gpn_node] using existing legacy node DB at {} (OS-default base path; \
                     set GPN_BASE_PATH to pin it — see docs/run-a-node.md)",
                    legacy.display()
                );
                return legacy;
            }
        }
    }
    pinned
}

/// What pruning flags (if any) gpn_node should append for the node child.
#[derive(Debug, PartialEq, Eq)]
enum PruningPlan {
    /// Fresh DB → request archive (+ write the marker).
    Archive,
    /// Existing DB with our marker → request archive (safe: we created it).
    ArchiveKeep,
    /// Existing legacy/unknown DB → request NO pruning flags (fall back to the
    /// node's compiled defaults / the DB's stored mode) so it can never
    /// hard-fail with `Incompatible pruning modes`.
    LeaveAlone,
    /// Operator set pruning explicitly (GPN_NODE_ARGS / node_args) → don't touch.
    Explicit,
}

/// Decide the pruning plan WITHOUT side effects (unit-testable).
fn pruning_plan(node_args: &[String], base: &Path) -> PruningPlan {
    if has_flag(node_args, "--state-pruning")
        || has_flag(node_args, "--blocks-pruning")
        || has_flag(node_args, "--pruning")
    {
        return PruningPlan::Explicit;
    }
    if db_exists_at(base) {
        if has_archive_marker(base) {
            PruningPlan::ArchiveKeep
        } else {
            PruningPlan::LeaveAlone
        }
    } else {
        PruningPlan::Archive
    }
}

/// Does the node-args list already contain this flag (exact or `flag=`)?
fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter()
        .any(|a| a == flag || a.starts_with(&format!("{flag}=")))
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
        let rpc_port = env_u16("GPN_RPC_PORT").or(file.rpc_port).unwrap_or(9944);

        Ok(Config {
            chain: env_str("GPN_CHAIN")
                .or(file.chain)
                .unwrap_or_else(|| "testnet".into()),
            signer: env_str("GPN_SIGNER")
                .or(file.signer)
                .unwrap_or_else(|| "//Alice".into()),
            account: env_str("GPN_ACCOUNT").or(file.account).unwrap_or_default(),
            endpoint,
            base_path: resolve_effective_base(
                env_str("GPN_BASE_PATH")
                    .or(file.base_path)
                    .map(|s| PathBuf::from(expand_tilde(&s))),
            ),
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
            // ── Optional providers: ON by design, opt OUT per capability ──────
            // A GPN node serves EVERY job type by default. The GPU still runs
            // only ONE job at a time (mutual exclusion ≠ a limit on job types).
            // A constrained box can disable a capability with `GPN_*=0` (env)
            // or `<capability> = false` (config file).
            diffusion: env_bool("GPN_DIFFUSION").or(file.diffusion).unwrap_or(true),
            diffusion_poll_ms: env_u64("GPN_DIFFUSION_POLL_MS")
                .or(file.diffusion_poll_ms)
                .unwrap_or(2000),
            ml: env_bool("GPN_ML").or(file.ml).unwrap_or(true),
            ml_poll_ms: env_u64("GPN_ML_POLL_MS")
                .or(file.ml_poll_ms)
                .unwrap_or(2000),
            finetune: env_bool("GPN_FINETUNE").or(file.finetune).unwrap_or(true),
            finetune_poll_ms: env_u64("GPN_FINETUNE_POLL_MS")
                .or(file.finetune_poll_ms)
                .unwrap_or(4000),
            bin_dir,
            status_secs: env_u64("GPN_STATUS_SECS")
                .or(file.status_secs)
                .unwrap_or(15),
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
    /// Exit code of the most recent crash, if it exited with one. Used to
    /// surface a distinct `down(not dialable)` state (node exit code 78).
    last_exit: Option<i32>,
}

type Shared = Arc<Mutex<BTreeMap<&'static str, ChildStatus>>>;

fn set_status(shared: &Shared, label: &'static str, up: bool, pid: u32) {
    let mut m = shared.lock().unwrap();
    let e = m.entry(label).or_default();
    e.up = up;
    e.pid = pid;
    if up {
        // A fresh (or restarted) child starts with a clean slate.
        e.last_exit = None;
    } else {
        e.restarts += 1;
    }
}

/// Record the exit code of a crashed child (drives `down(not dialable)`).
fn set_exit_code(shared: &Shared, label: &'static str, code: Option<i32>) {
    let mut m = shared.lock().unwrap();
    if let Some(e) = m.get_mut(label) {
        e.last_exit = code;
    }
}

/// Exit code the node uses when the dialability gate fails (`EXIT_NOT_DIALABLE`,
/// sysexits `EX_CONFIG`). Keep in sync with `gpn-node`'s
/// `dialability::EXIT_NOT_DIALABLE`.
const NODE_EXIT_NOT_DIALABLE: i32 = 78;

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
                Ok(l) => asciiprintln!("{prefix} {l}"),
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
                log_event(
                    label,
                    &format!("spawn failed: {e} — retrying in {backoff_ms}ms"),
                );
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
        let code = status.as_ref().and_then(|st| st.code());
        let detail = match status {
            Some(st) => st
                .code()
                .map(|c| format!("exit code {c}"))
                .unwrap_or_else(|| "terminated by signal".to_string()),
            None => "unknown".to_string(),
        };
        if code == Some(NODE_EXIT_NOT_DIALABLE) {
            log_event(
                label,
                "NOT DIALABLE: the node refused to join the network (no inbound/reachable public \
                 address). Set `public_addr` / GPN_PUBLIC_ADDR to /ip4/<public-ip>/tcp/<port> and \
                 forward that port on the router. Retrying with backoff — the status line shows \
                 `node=down(not dialable)`.",
            );
        }
        log_event(
            label,
            &format!(
                "exited ({detail}) after {}s — restarting in {backoff_ms}ms",
                ran.as_secs()
            ),
        );
        set_status(&shared, label, false, 0);
        set_exit_code(&shared, label, code);

        // A child that had been healthy for a while → reset the backoff —
        // EXCEPT for a not-dialable exit, which is a configuration error that
        // will persist until the operator forwards the port.
        if ran.as_secs() >= 60 && code != Some(NODE_EXIT_NOT_DIALABLE) {
            backoff_ms = 1000;
        }
        // A not-dialable node will stay not-dialable until the operator fixes
        // the port forward; use the existing bounded backoff but never the
        // tight 1-2 s ramp that would hammer the router/NAT.
        if code == Some(NODE_EXIT_NOT_DIALABLE) {
            backoff_ms = backoff_ms.max(30_000);
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
    asciiprintln!("[supervisor {ts}] {label}: {msg}");
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

/// Cheaply read the node's connected-peer count from its own HTTP JSON-RPC.
fn peer_count(rpc_port: u16) -> Option<u64> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(1200))
        .build()
        .ok()?;
    let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"system_health","params":[]});
    let resp: serde_json::Value = client
        .post(format!("http://127.0.0.1:{rpc_port}"))
        .json(&body)
        .send()
        .ok()?
        .json()
        .ok()?;
    resp.get("result")?.get("peers")?.as_u64()
}

/// Sleep `ms` in small steps so shutdown stays prompt (returns early if set).
fn sleep_steps(shutdown: &Arc<AtomicBool>, ms: u64) {
    let mut left = ms;
    while left > 0 && !shutdown.load(Ordering::SeqCst) {
        let s = 200.min(left);
        std::thread::sleep(Duration::from_millis(s));
        left -= s;
    }
}

/// Sync-stall watchdog ("never silently wedge again"). GPN has no finality, so a
/// node can stay p2p-connected while its best block silently freezes (a
/// state/block request loop → the sync peer bans it → it can't sync from its
/// only peer). If the node's best is unchanged for `GPN_STALL_SECS` (default
/// 300s) while ≥1 peer is connected, log a clear alert and restart the node
/// child (via SIGTERM — the supervisor then restarts it with backoff, and it
/// re-syncs). Normal syncing advances `best` every check, so it never triggers;
/// a node whose RPC isn't answering yet resets the clock rather than counting.
fn spawn_stall_watchdog(
    cfg: &Config,
    shared: Shared,
    shutdown: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    let port = cfg.rpc_port;
    std::thread::spawn(move || {
        let stall = Duration::from_secs(env_u64("GPN_STALL_SECS").unwrap_or(300).max(60));
        let check = Duration::from_secs(env_u64("GPN_STALL_CHECK_SECS").unwrap_or(60).max(10));
        let mut last_best: Option<u64> = None;
        let mut last_change = Instant::now();
        while !shutdown.load(Ordering::SeqCst) {
            sleep_steps(&shutdown, check.as_millis() as u64);
            if shutdown.load(Ordering::SeqCst) {
                break;
            }
            match best_block(port) {
                None => {
                    // RPC not ready (starting / restarting) — don't count as stall.
                    last_change = Instant::now();
                }
                Some(b) => {
                    if last_best != Some(b) {
                        last_best = Some(b);
                        last_change = Instant::now();
                    } else if last_change.elapsed() >= stall {
                        let (node_up, pid) = {
                            let m = shared.lock().unwrap();
                            m.get("node").map(|s| (s.up, s.pid)).unwrap_or((false, 0))
                        };
                        let peers = peer_count(port).unwrap_or(0);
                        if node_up && peers >= 1 && pid != 0 {
                            log_event(
                                "watchdog",
                                &format!(
                                    "SYNC STALL: best frozen at #{b} for {}s with {peers} peer(s) connected \
                                     — restarting node child (pid {pid}) to re-sync (GPN has no finality; see \
                                     docs/run-a-node.md)",
                                    last_change.elapsed().as_secs()
                                ),
                            );
                            send_term(pid);
                        } else {
                            log_event(
                                "watchdog",
                                &format!(
                                    "best frozen at #{b} for {}s but peers={peers} node_up={node_up} — not restarting",
                                    last_change.elapsed().as_secs()
                                ),
                            );
                        }
                        // Reset so we don't immediately re-trigger while it restarts.
                        last_change = Instant::now();
                    }
                }
            }
        }
        log_event("watchdog", "stall watchdog exiting");
    })
}

fn status_line(cfg: &Config, shared: &Shared) -> String {
    let m = shared.lock().unwrap();
    let mut parts: Vec<String> = Vec::new();
    for label in ["node", "heartbeat", "provider", "diffuse", "ml", "finetune"] {
        if let Some(st) = m.get(label) {
            if st.up {
                parts.push(format!("{label}=up(pid {})", st.pid));
            } else if st.last_exit == Some(NODE_EXIT_NOT_DIALABLE) {
                parts.push(format!("{label}=down(not dialable, restarts {})", st.restarts));
            } else {
                parts.push(format!("{label}=down(restarts {})", st.restarts));
            }
        }
    }
    let mut line = format!("[status] children={}", parts.len());
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
        // Mining is structural: `gpn-node` is an authority + authoring miner by
        // default (see service::new_full). We deliberately do NOT pass
        // `--force-authoring` here — a plain node launch must mine on its own.
        argv.push("--rpc-port".into());
        argv.push(cfg.rpc_port.to_string());
        if let Some(a) = &cfg.author {
            argv.push(format!("--{a}"));
        }
        if let Some(k) = &cfg.node_key {
            argv.push("--node-key".into());
            argv.push(k.clone());
        }
        // Always pass `--base-path` — the SAME path the DB-existence check
        // uses, so the guard can never inspect a different dir than the node.
        argv.push("--base-path".into());
        argv.push(cfg.base_path.to_string_lossy().into_owned());
        for bn in &cfg.bootnodes {
            argv.push("--bootnodes".into());
            argv.push(bn.clone());
        }
        if let Some(pa) = &cfg.public_addr {
            argv.push("--public-addr".into());
            argv.push(pa.clone());
        }

        // ── Sync-robustness defaults (see docs/run-a-node.md, "node connected
        //    but not syncing"). GPN has NO finality gadget (finalized head stays
        //    #0). That breaks the two things a node relies on to sync:
        //      * the *finality-relative* default pruning (state-pruning=256,
        //        blocks-pruning=archive-canonical) discards state/bodies almost
        //        immediately, so a peer that asks for them gets nothing →
        //        "State already discarded" → the peer bans us → best freezes; and
        //      * the fast/warp sync modes need a finalized block for their proof.
        //    So we default to the one reliable combination and keep it
        //    overridable (GPN_NODE_ARGS / node_args wins):
        //      --sync full
        //      --state-pruning archive --blocks-pruning archive
        //    Archive is cheap here (~0.4 GB testnet DB) and is the only mode that
        //    never discards served state on a chain with no finality.
        //
        //    CAVEAT: Substrate locks the state-pruning mode into the DB at first
        //    creation; requesting `archive` on a legacy `256` DB HARD-FAILS
        //    ("Incompatible pruning modes"). So apply the archive default only to
        //    a FRESH db dir, or to a dir WE previously created with archive (a
        //    `.gpn-pruning-archive` marker). On any legacy/unknown existing DB we
        //    request NO pruning flags (fall back to the node's compiled defaults,
        //    which match the stored mode) and warn — the node must never
        //    crash-loop on `Incompatible pruning modes`.
        let plan = pruning_plan(&cfg.node_args, &cfg.base_path);
        if !has_flag(&cfg.node_args, "--sync") {
            argv.push("--sync".into());
            argv.push("full".into());
        }
        match plan {
            PruningPlan::Archive | PruningPlan::ArchiveKeep => {
                if matches!(plan, PruningPlan::Archive) {
                    write_archive_marker(&cfg.base_path);
                }
                argv.push("--state-pruning".into());
                argv.push("archive".into());
                argv.push("--blocks-pruning".into());
                argv.push("archive".into());
            }
            PruningPlan::LeaveAlone => {
                log_event(
                    "node",
                    &format!(
                        "existing DB at {} has no archive marker — leaving its pruning mode untouched \
                         (requesting archive would hard-fail: Substrate locks pruning at DB creation). \
                         A legacy 256-pruned DB cannot serve state on a finality-less chain; re-create it \
                         (wipe chains/, or import a fresh *archive* snapshot) for stall-proof syncing — see \
                         docs/run-a-node.md",
                        cfg.base_path.display()
                    ),
                );
            }
            PruningPlan::Explicit => {}
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
        // Advertise EVERY capability this node serves so the scheduler routes
        // LLM/image/video/ML/fine-tune jobs to it. One GPU still runs one job
        // at a time (mutual exclusion), but the node is equipped for them all.
        let mut caps: Vec<&str> = vec!["LLMInference"];
        if cfg.diffusion {
            caps.push("ImageGeneration");
            caps.push("VideoGeneration");
        }
        if cfg.ml {
            caps.push("MLInference");
        }
        if cfg.finetune {
            caps.push("FineTuning");
        }
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
            "--capabilities".into(),
            caps.join(","),
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
            if optional_provider_available(cfg, "gpn_diffusion_provider") {
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
            } else {
                log_event(
                    "diffuse",
                    "gpn_diffusion_provider not found — image/video capability DISABLED on this node \n                     (node + other providers stay healthy). Install the provider binary or set GPN_DIFFUSION=0 to silence this.",
                );
            }
        }

        // ML-inference provider (job type `MLInference`) — ON by default.
        if cfg.ml {
            if optional_provider_available(cfg, "gpn_ml_provider") {
                specs.push(ChildSpec {
                    label: "ml",
                    prefix: "[ml]",
                    argv: vec![
                        cfg.resolve("gpn_ml_provider"),
                        cfg.endpoint.clone(),
                        cfg.signer.clone(),
                        cfg.ml_poll_ms.to_string(),
                    ],
                    envs: vec![
                        ("SIGNER_URI".to_string(), cfg.signer.clone()),
                        ("OLLAMA_URL".to_string(), cfg.ollama.clone()),
                    ],
                });
            } else {
                log_event(
                    "ml",
                    "gpn_ml_provider not found — ML-inference capability DISABLED on this node \n                     (node + other providers stay healthy). Install the provider binary or set GPN_ML=0 to silence this.",
                );
            }
        }

        // Fine-tune provider (job type `FineTuning`) — ON by default.
        if cfg.finetune {
            if optional_provider_available(cfg, "gpn_finetune_provider") {
                specs.push(ChildSpec {
                    label: "finetune",
                    prefix: "[finetune]",
                    argv: vec![
                        cfg.resolve("gpn_finetune_provider"),
                        cfg.endpoint.clone(),
                        cfg.signer.clone(),
                        cfg.finetune_poll_ms.to_string(),
                    ],
                    envs: vec![("SIGNER_URI".to_string(), cfg.signer.clone())],
                });
            } else {
                log_event(
                    "finetune",
                    "gpn_finetune_provider not found — fine-tune capability DISABLED on this node \n                     (node + other providers stay healthy). Install the provider binary or set GPN_FINETUNE=0 to silence this.",
                );
            }
        }
    }

    specs
}

/// Is an *optional* provider binary present (in `bin_dir`, or on `$PATH`)?
///
/// A missing optional provider must be **non-fatal**: we log a clear warning
/// and simply don't start that child, rather than crash-loop it. The node and
/// its other providers stay fully healthy — a box without (say) the diffusion
/// binary still mines and serves LLM/ML/fine-tune jobs.
fn optional_provider_available(cfg: &Config, name: &str) -> bool {
    if cfg.bin_dir.join(name).is_file() {
        return true;
    }
    if cfg.bin_dir.join(format!("{name}.exe")).is_file() {
        return true;
    }
    on_path(name)
}

/// Whether `name` (or `name.exe`) resolves on `$PATH`.
fn on_path(name: &str) -> bool {
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            if dir.join(name).is_file() || dir.join(format!("{name}.exe")).is_file() {
                return true;
            }
        }
    }
    false
}

/// Report whether an ollama server answers at `url` (short timeout). Used to
/// warn clearly at startup when the box can mine but can't serve LLMs yet.
fn ollama_reachable(url: &str) -> bool {
    let base = url.trim_end_matches('/');
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(1500))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    client
        .get(format!("{base}/api/tags"))
        .send()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

#[tokio::main]
async fn main() {
    env_logger::init();
    set_ascii_mode(ascii_mode_default(&std::env::args().collect::<Vec<_>>()));
    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            asciieprintln!("gpn_node: {e}");
            std::process::exit(2);
        }
    };

    asciiprintln!("gpn_node — single-node supervisor (a node is always miner + AI provider)");
    asciiprintln!(
        "  chain={} endpoint={} rpc_port={} bin_dir={}",
        cfg.chain,
        cfg.endpoint,
        cfg.rpc_port,
        cfg.bin_dir.display()
    );
    if cfg.account.is_empty() {
        asciiprintln!("  account=<unset> (payout address not advertised)");
    } else {
        asciiprintln!("  account={}", cfg.account);
    }
    asciiprintln!(
        "  heartbeat listen={} ollama={} gflops={}",
        cfg.listen,
        cfg.ollama,
        cfg.gflops
    );

    // ── Ollama is a first-class prerequisite: without it the node still mines,
    //    but the provider cannot serve any LLM job. Warn clearly & actionably. ──
    if !ollama_reachable(&cfg.ollama) {
        asciieprintln!(
            "\n  ⚠️  ollama NOT reachable at {} — the node will MINE, but it CANNOT serve LLM jobs until ollama is running.",
            cfg.ollama
        );
        asciieprintln!("      Install & start ollama (one-time):");
        asciieprintln!(
            "        Windows :  winget install Ollama.Ollama   (or  scripts\\install-ollama.ps1 )"
        );
        asciieprintln!("        Linux   :  scripts/install-ollama.sh      (or  curl -fsSL https://ollama.com/install.sh | sh )");
        asciieprintln!(
            "      Then:  ollama serve        # keep it running (listens on 127.0.0.1:11434)"
        );
        asciieprintln!("             ollama pull llama3.2:1b");
        asciieprintln!("      Docs: docs/run-a-node.md\n");
    } else {
        asciiprintln!("  ollama: reachable at {}", cfg.ollama);
    }

    let specs = build_children(&cfg);
    if specs.is_empty() {
        asciieprintln!("gpn_node: no children to run");
        std::process::exit(2);
    }
    let labels: Vec<&'static str> = specs.iter().map(|s| s.label).collect();
    asciiprintln!("  children: {}", labels.join(", "));

    let shared: Shared = Arc::new(Mutex::new(BTreeMap::new()));
    let shutdown = Arc::new(AtomicBool::new(false));

    // One supervisor thread per child.
    let mut handles = Vec::new();
    for spec in specs {
        let shared = shared.clone();
        let shutdown = shutdown.clone();
        handles.push(std::thread::spawn(move || {
            supervise(spec, shared, shutdown)
        }));
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
            asciiprintln!("{}", status_line(&status_cfg, &status_shared));
        }
    });

    // Sync-stall watchdog (auto-recover a silently wedged node).
    let watchdog_handle = spawn_stall_watchdog(&cfg, shared.clone(), shutdown.clone());

    // Wait for Ctrl-C or SIGTERM.
    wait_for_signal().await;
    asciiprintln!("\n[supervisor] shutdown requested — stopping children…");
    shutdown.store(true, Ordering::SeqCst);

    for h in handles {
        let _ = h.join();
    }
    let _ = status_handle.join();
    let _ = watchdog_handle.join();
    asciiprintln!("[supervisor] all children stopped — exiting");
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

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal unique temp dir (no external crate).
    fn td(tag: &str) -> PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p =
            std::env::temp_dir().join(format!("gpn-node-test-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
    fn mk_db(base: &Path) {
        std::fs::create_dir_all(base.join("chains/gpn_testnet/db/full")).unwrap();
    }
    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn fresh_dir_requests_archive() {
        let b = td("fresh");
        assert!(!db_exists_at(&b));
        assert_eq!(pruning_plan(&[], &b), PruningPlan::Archive);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn legacy_db_is_left_alone() {
        let b = td("legacy");
        mk_db(&b);
        // No marker → legacy/unknown (e.g. old --state-pruning 256) → do NOT
        // force archive (that would crash-loop 'Incompatible pruning modes').
        assert_eq!(pruning_plan(&[], &b), PruningPlan::LeaveAlone);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn marked_archive_db_keeps_archive() {
        let b = td("marked");
        mk_db(&b);
        write_archive_marker(&b);
        assert_eq!(pruning_plan(&[], &b), PruningPlan::ArchiveKeep);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn explicit_node_args_respected() {
        let b = td("explicit");
        mk_db(&b); // even with an existing DB, explicit flags win
        assert_eq!(
            pruning_plan(&args(&["--state-pruning", "256"]), &b),
            PruningPlan::Explicit
        );
        assert_eq!(
            pruning_plan(&args(&["--pruning=256"]), &b),
            PruningPlan::Explicit
        );
        assert_eq!(
            pruning_plan(&args(&["--blocks-pruning", "archive"]), &b),
            PruningPlan::Explicit
        );
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn legacy_os_default_db_is_detected() {
        // Pinned path empty, legacy OS-default path has a DB → use the legacy.
        let pinned = td("pinned-empty");
        let legacy = td("legacy-os");
        mk_db(&legacy);
        assert_eq!(
            resolve_effective_base_with(Some(pinned.clone()), Some(legacy.clone())),
            legacy
        );
        let _ = std::fs::remove_dir_all(&pinned);
        let _ = std::fs::remove_dir_all(&legacy);
    }

    #[test]
    fn pinned_db_wins_over_empty_legacy() {
        let pinned = td("pinned-db");
        mk_db(&pinned);
        let legacy = td("legacy-empty");
        assert_eq!(
            resolve_effective_base_with(Some(pinned.clone()), Some(legacy.clone())),
            pinned
        );
        let _ = std::fs::remove_dir_all(&pinned);
        let _ = std::fs::remove_dir_all(&legacy);
    }

    #[test]
    fn default_base_path_is_pinned() {
        // Sanity: a pinned default exists on both platforms (never empty).
        assert!(!default_base_path().as_os_str().is_empty());
    }

    #[test]
    fn ascii_no_emoji_flag_forces_ascii() {
        // `--no-emoji` is deterministic regardless of the host OS / env.
        assert!(ascii_mode_default(&args(&["--no-emoji"])));
    }

    #[test]
    fn ascii_safe_leaves_ascii_untouched() {
        assert_eq!(to_ascii_safe("plain ascii 123"), "plain ascii 123");
    }

    #[test]
    fn ascii_safe_replaces_emoji_and_keeps_text() {
        // The mojibake sources: warning sign + variation selector, and emoji.
        assert_eq!(to_ascii_safe("\u{26a0}\u{fe0f}  ollama"), "!  ollama");
        assert_eq!(to_ascii_safe("sleep \u{1f4a4} now"), "sleep [zzz] now");
        // Unknown emoji → `?` (still ASCII, never mojibake).
        assert_eq!(to_ascii_safe("trophy \u{1f3c6}"), "trophy [win]");
        assert_eq!(to_ascii_safe("mystery \u{1f47d}"), "mystery ?");
    }

    #[test]
    fn ascii_safe_output_is_always_ascii() {
        let s = "\u{26a0}\u{fe0f} \u{1f680} \u{2705} \u{2014} \u{2026} \u{2022}";
        assert!(to_ascii_safe(s).is_ascii());
    }
}
