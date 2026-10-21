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
//! **Ollama, however, is REQUIRED.** A node is miner *and* AI provider, so
//! `gpn_node` probes `GPN_OLLAMA` at startup, attempts the automated bootstrap
//! (install → `ollama serve` → pull the baseline), and **refuses to start**
//! (exit 69, `node=down(no ollama)`) if ollama is still unreachable.
//! `gpn_node --setup` runs that bootstrap explicitly; `GPN_REQUIRE_OLLAMA=0`
//! is the documented opt-out (degraded, mining-only). See `docs/run-a-node.md`.
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

use gpn_agent::ollama;
use gpn_agent::update;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, IsTerminal};
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
    /// Path to an sr25519 JSON keystore (owner-only). Written by `--setup`
    /// and read silently on every later start; never contains the secret in
    /// this file.
    signer_file: Option<String>,
    account: Option<String>,
    endpoint: Option<String>,
    base_path: Option<String>,
    rpc_port: Option<u16>,
    node_key: Option<String>,
    bootnodes: Option<Vec<String>>,
    /// Published seed-list URLs (`--bootnodes-url`). Empty → the node uses its
    /// built-in default, the live directory at https://gpncoin.ai/seeds.json.
    bootnodes_url: Option<Vec<String>>,
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
    /// Informational only: the operator intends to install a service unit.
    autostart: Option<bool>,
    /// How often the aggregated status line is printed (seconds).
    status_secs: Option<u64>,
    /// Opt in to automatic updates (`GPN_AUTO_UPDATE`). Default **false**.
    auto_update: Option<bool>,
    /// Signed release manifest URL (`GPN_UPDATE_URL`).
    update_url: Option<String>,
    /// Hours between auto-update checks (`GPN_UPDATE_CHECK_HOURS`).
    update_check_hours: Option<u64>,
    /// Restart automatically to apply a staged update (`GPN_UPDATE_RESTART`).
    update_restart: Option<bool>,
    /// Fetch the manifest at all (`GPN_UPDATE_CHECK=0` for air-gapped hosts).
    update_check: Option<bool>,
    /// Enforce `min_version` by refusing to mine/announce (`GPN_MIN_VERSION_ENFORCE`).
    min_version_enforce: Option<bool>,
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
    bootnodes_url: Vec<String>,
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
    /// Informational: the config asks for boot-start.
    autostart: bool,
    /// Opt-in auto-update (never on unless the operator asked).
    auto_update: bool,
    /// Signed release manifest URL.
    update_url: String,
    /// Hours between update checks.
    update_check_hours: u64,
    /// Restart automatically to apply a staged update (a short outage).
    update_restart: bool,
    /// Whether to fetch the manifest at all (off = air-gapped).
    update_check: bool,
    /// Whether `min_version` is enforced (emergency escape hatch, default on).
    min_version_enforce: bool,
    /// Set at startup when this build is below `min_version`: sync, don't
    /// mine, don't announce.
    standby: bool,
    /// The config file that was read (None when defaults are in use).
    config_path: Option<PathBuf>,
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

/// `asciiprint!` (no newline) that honours ASCII mode.
macro_rules! asciiprint {
    ($($arg:tt)*) => {{
        let line = format!($($arg)*);
        if crate::ascii_mode() {
            ::std::print!("{}", crate::to_ascii_safe(&line));
        } else {
            ::std::print!("{line}");
        }
        let _ = ::std::io::Write::flush(&mut ::std::io::stdout());
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
    gpn_agent::firstrun::default_base_path()
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
    gpn_agent::firstrun::expand_tilde(p)
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

        let (file, config_path) = match &cfg_path {
            Some(p) => {
                let p = expand_tilde(p);
                let text = std::fs::read_to_string(&p)
                    .map_err(|e| format!("cannot read config '{p}': {e}"))?;
                let parsed = toml::from_str::<FileConfig>(&text)
                    .map_err(|e| format!("invalid config '{p}': {e}"))?;
                (parsed, Some(PathBuf::from(p)))
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
                        let parsed = toml::from_str::<FileConfig>(&text)
                            .map_err(|e| format!("invalid config '{p}': {e}"))?;
                        (parsed, Some(PathBuf::from(p)))
                    }
                    None => (FileConfig::default(), None),
                }
            }
        };

        // bin dir: env GPN_BIN > file > dir of this exe > $HOME/.local/bin
        let bin_dir = env_str("GPN_BIN")
            .or(file.bin_dir.clone())
            .map(|s| PathBuf::from(expand_tilde(&s)))
            .or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(PathBuf::from))
            })
            .unwrap_or_else(|| PathBuf::from(expand_tilde("~/.local/bin")));

        let endpoint = env_str("GPN_ENDPOINT")
            .or(file.endpoint.clone())
            .unwrap_or_else(|| "ws://127.0.0.1:9944".to_string());
        let rpc_port = env_u16("GPN_RPC_PORT").or(file.rpc_port).unwrap_or(9944);

        // ── 1. Base path: explicit → config → OS default (created) ─────────
        // `~/.gpn/node` (Unix) / `C:\gpn\data` (Windows) holds the chain DB
        // AND the wallet — the one directory that makes a second run
        // zero-config. It is created here, before anything needs it.
        let explicit_base = env_str("GPN_BASE_PATH")
            .or(file.base_path.clone())
            .map(|s| PathBuf::from(expand_tilde(&s)));
        let base_is_default = explicit_base.is_none();
        let base_path = resolve_effective_base(explicit_base);
        std::fs::create_dir_all(&base_path)
            .map_err(|e| format!("cannot create base path {}: {e}", base_path.display()))?;
        if base_is_default {
            asciiprintln!(
                "[gpn_node] base path: {} (node database + wallet — where GPN keeps its data; default)",
                base_path.display()
            );
        }

        // ── 2. Chain spec: explicit → <base>/testnet-spec.json → auto-fetch ─
        // A first run with no GPN_CHAIN downloads the canonical spec from the
        // public access point and pins it in the base dir (no prompt).
        let chain = gpn_agent::firstrun::resolve_chain(
            env_str("GPN_CHAIN").or(file.chain.clone()),
            &base_path,
        )
        .chain;

        // ── 3. Signer: env → config signer → keystore env/config → default
        //       keystore → first-run wizard (or a clear non-TTY refusal). ────
        let keystore_path = gpn_agent::firstrun::default_keystore_path(&base_path);
        let configured_keystore = env_str("GPN_SIGNER_FILE")
            .or(file.signer_file.clone())
            .map(|s| PathBuf::from(expand_tilde(&s)));
        let signer = if let Some(uri) = env_str("GPN_SIGNER").or(file.signer.clone()) {
            uri
        } else if let Some(path) = configured_keystore
            .clone()
            .or_else(|| keystore_path.is_file().then(|| keystore_path.clone()))
        {
            gpn_agent::firstrun::secret_from_keystore(&path)?
        } else if chain == "dev" || chain == "local" || chain.is_empty() {
            // Dev chains are throwaway: the well-known dev key is the default
            // (and the wizard is never triggered for them).
            "//Alice".to_string()
        } else {
            let is_tty = std::io::stdin().is_terminal();
            gpn_agent::firstrun::ensure_interactive(is_tty)?;
            asciiprintln!(
                "[gpn_node] no wallet configured — starting the one-time wallet setup \
                 (stores {}; owner-only)",
                keystore_path.display()
            );
            let mut stdin = std::io::stdin().lock();
            let mut stdout = std::io::stdout();
            gpn_agent::firstrun::wallet_wizard(&mut stdin, &mut stdout, &keystore_path)?.mnemonic
        };

        // Payout account: derived from the wallet when not set explicitly, so
        // earnings can never be sent to a default/placeholder address.
        let mut account = env_str("GPN_ACCOUNT")
            .or(file.account.clone())
            .unwrap_or_default();
        if account.is_empty() {
            if let Ok(addr) = gpn_agent::firstrun::ss58_of(&signer) {
                account = addr;
            }
        }

        // ── 4. Zero-config second run: write the config file once. ──────────
        // Only when no config file existed (never overwrite an operator's
        // file) and never at a `--config` path we were told to read.
        if config_path.is_none() {
            let prefix = expand_tilde(&env_str("GPN_PREFIX").unwrap_or_else(|| "~/.gpn".into()));
            let target = PathBuf::from(prefix).join("node.toml");
            let body = gpn_agent::firstrun::config_toml(
                &base_path,
                &chain,
                keystore_path.is_file().then_some(keystore_path.as_path()),
            );
            match gpn_agent::firstrun::write_config_if_missing(&target, &body) {
                Ok(true) => asciiprintln!(
                    "[gpn_node] config written: {} (second run is zero-config)",
                    target.display()
                ),
                Ok(false) => {}
                Err(e) => asciieprintln!("[gpn_node] ! could not write config: {e}"),
            }
        }

        Ok(Config {
            chain,
            signer,
            account,
            endpoint,
            base_path,
            rpc_port,
            node_key: env_str("GPN_NODE_KEY").or(file.node_key),
            bootnodes: env_str("GPN_BOOTNODES")
                .map(|s| split_list(&s))
                .or(file.bootnodes)
                .unwrap_or_default(),
            bootnodes_url: env_str("GPN_BOOTNODES_URL")
                .or_else(|| env_str("GPN_SEEDS_URL"))
                .map(|s| split_list(&s))
                .or(file.bootnodes_url)
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
            autostart: env_bool("GPN_AUTOSTART")
                .or(file.autostart)
                .unwrap_or(false),
            // ── Updates: opt-in. Nothing is downloaded unless asked. ──────────
            auto_update: match env_str("GPN_AUTO_UPDATE") {
                Some(v) => update::auto_update_enabled(Some(&v)),
                None => file.auto_update.unwrap_or(false),
            },
            update_url: env_str("GPN_UPDATE_URL")
                .or(file.update_url)
                .unwrap_or_else(|| update::MANIFEST_URL.to_string()),
            update_check_hours: env_u64("GPN_UPDATE_CHECK_HOURS")
                .or(file.update_check_hours)
                .unwrap_or(6),
            update_restart: env_bool("GPN_UPDATE_RESTART")
                .or(file.update_restart)
                .unwrap_or(true),
            update_check: env_bool("GPN_UPDATE_CHECK")
                .or(file.update_check)
                .unwrap_or(true),
            min_version_enforce: env_bool("GPN_MIN_VERSION_ENFORCE")
                .or(file.min_version_enforce)
                .unwrap_or(true),
            standby: false,
            config_path,
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

    /// Where updater bookkeeping lives (staged marker, cached cutoff).
    fn update_dir(&self) -> PathBuf {
        self.base_path.join("update")
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

// ─────────────────────────────────────────────────────────────────────────────
// Updates: signed manifest, opt-in auto-update, minimum-version cutoff
// ─────────────────────────────────────────────────────────────────────────────

fn user_agent() -> String {
    format!("gpn_node/{} ({}/{})", env!("CARGO_PKG_VERSION"), update::platform_os(), update::platform_arch())
}

fn http_client(timeout: Duration) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .user_agent(user_agent())
        .build()
        .map_err(|e| e.to_string())
}

/// Fetch the manifest and **verify the signature** before anything else is done
/// with it. There is no unverified path: a manifest that fails verification is
/// an error, never a "best effort".
fn fetch_verified_manifest(cfg: &Config) -> Result<update::ReleaseManifest, String> {
    let client = http_client(Duration::from_secs(15))?;
    let resp = client
        .get(&cfg.update_url)
        .send()
        .map_err(|e| format!("{}: {e}", cfg.update_url))?;
    if !resp.status().is_success() {
        return Err(format!("{} -> HTTP {}", cfg.update_url, resp.status()));
    }
    let body = resp.bytes().map_err(|e| e.to_string())?;
    update::verify_manifest(&body, update::RELEASE_PUBKEY_HEX).map_err(|e| e.to_string())
}

fn download_asset(url: &str, timeout: Duration) -> Result<Vec<u8>, String> {
    let client = http_client(timeout)?;
    let resp = client.get(url).send().map_err(|e| format!("download {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download {url} -> HTTP {}", resp.status()));
    }
    let bytes = resp.bytes().map_err(|e| format!("download body: {e}"))?;
    Ok(bytes.to_vec())
}

/// The loud, unmissable "this build is too old" block.
fn log_upgrade_required(current: &str, min: &str, enforcing: bool) {
    asciieprintln!("");
    asciieprintln!("  ============================================================");
    asciieprintln!("  !! UPGRADE REQUIRED — this node is {current}, the network requires >= {min}");
    asciieprintln!("  ============================================================");
    if enforcing {
        asciieprintln!("  This node will SYNC (so its data stays useful) but will NOT MINE and");
        asciieprintln!("  will NOT ANNOUNCE itself. An out-of-date node can otherwise produce blocks");
        asciieprintln!("  the rest of the network rejects — and that looks like a chain stall.");
        asciieprintln!("  Fix: install the current release from https://gpncoin.ai/downloads");
        asciieprintln!("       (or set GPN_AUTO_UPDATE=1 and restart — it updates itself).");
    } else {
        asciieprintln!("  GPN_MIN_VERSION_ENFORCE=0 — mining anyway. This is NOT supported:");
        asciieprintln!("  blocks this node seals may be rejected by upgraded peers.");
    }
    asciieprintln!("");
}

/// Run a blocking job (the updater uses `reqwest::blocking`) on a plain thread.
///
/// `reqwest::blocking` builds its own runtime and **panics if it is created or
/// dropped inside an async context** ("Cannot drop a runtime in a context where
/// blocking is not allowed"). Both the supervisor's `main` and the `--setup`
/// path are inside `#[tokio::main]`, so every blocking call must go through a
/// thread — exactly like the ollama gate does.
fn on_plain_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static, fallback: T) -> T {
    std::thread::spawn(f).join().unwrap_or(fallback)
}

/// One manifest check at startup.
///
/// Sets `cfg.standby` when this build is below the signed `min_version`. A
/// network failure is never fatal: the last verified cutoff is cached on disk
/// and keeps applying while offline.
fn startup_update_check(cfg: &mut Config) {
    let dir = cfg.update_dir();
    if !cfg.update_check {
        asciiprintln!(
            "  update: manifest checks DISABLED (GPN_UPDATE_CHECK=0) — the minimum-version \
             cutoff cannot be evaluated on this host"
        );
        return;
    }

    let cached = update::load_state(&dir);
    let mut min_version = cached
        .as_ref()
        .map(|s| s.min_version.clone())
        .unwrap_or_default();
    let mut latest = cached
        .as_ref()
        .map(|s| s.latest_version.clone())
        .unwrap_or_default();

    match fetch_verified_manifest(cfg) {
        Ok(m) => {
            asciiprintln!(
                "  update: manifest verified ({}): latest={} min={}",
                cfg.update_url,
                m.version,
                m.min_version
            );
            min_version = m.min_version.clone();
            latest = m.version.clone();
            let staged = update::read_pending(&dir).map(|p| p.version);
            let _ = update::save_state(
                &dir,
                &update::UpdateState {
                    last_check: update::now_unix(),
                    latest_version: latest.clone(),
                    min_version: min_version.clone(),
                    staged_version: staged,
                },
            );
        }
        Err(e) => {
            asciieprintln!(
                "  update: manifest check failed ({e}) — keeping the last verified cutoff \
                 '{min_version}'"
            );
        }
    }

    if update::min_version_decision(env!("CARGO_PKG_VERSION"), &min_version)
        == update::Support::UpgradeRequired
    {
        log_upgrade_required(env!("CARGO_PKG_VERSION"), &min_version, cfg.min_version_enforce);
        cfg.standby = cfg.min_version_enforce;
    } else if !latest.is_empty() && update::is_newer(&latest, env!("CARGO_PKG_VERSION")) {
        asciiprintln!(
            "  update: {latest} is available (this node is {}); auto-update is {}",
            env!("CARGO_PKG_VERSION"),
            if cfg.auto_update { "ON" } else { "off" }
        );
    }
}

/// Download → verify in memory → stage on disk → verify on disk.
fn download_and_stage(
    cfg: &Config,
    m: &update::ReleaseManifest,
) -> Result<update::PendingUpdate, String> {
    let asset = update::select_asset(m, update::platform_os(), update::platform_arch())
        .map_err(|e| e.to_string())?;
    log_event(
        "update",
        &format!(
            "downloading {} ({}/{}) sha256={} size={}",
            m.version,
            asset.os,
            asset.arch,
            asset.sha256,
            asset.size
        ),
    );
    let bytes = download_asset(&asset.url, Duration::from_secs(300))?;
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let pending = update::stage_bytes(&cfg.update_dir(), &exe, &m.version, asset, &bytes)
        .map_err(|e| e.to_string())?;
    log_event(
        "update",
        &format!(
            "staged {} at {} — signature OK, sha256 verified twice",
            pending.version, pending.staged
        ),
    );
    Ok(pending)
}

/// One full check cycle (used at startup-with-auto-update and on a timer).
fn update_once(cfg: &Config, restart: &AtomicBool) -> Result<(), String> {
    let m = fetch_verified_manifest(cfg)?;

    // The cutoff can tighten between checks as well as at start.
    if update::min_version_decision(env!("CARGO_PKG_VERSION"), &m.min_version)
        == update::Support::UpgradeRequired
    {
        log_upgrade_required(env!("CARGO_PKG_VERSION"), &m.min_version, cfg.min_version_enforce);
    }

    if !update::is_newer(&m.version, env!("CARGO_PKG_VERSION")) {
        return Ok(());
    }
    let pending = download_and_stage(cfg, &m)?;

    // Cache the staged version so a restart knows what to expect.
    let _ = update::save_state(
        &cfg.update_dir(),
        &update::UpdateState {
            last_check: update::now_unix(),
            latest_version: m.version.clone(),
            min_version: m.min_version.clone(),
            staged_version: Some(pending.version.clone()),
        },
    );

    if cfg.update_restart {
        log_event(
            "update",
            &format!(
                "restarting to apply {} — this is a SHORT OUTAGE: children stop cleanly, the \
                 binary is swapped atomically and the node comes back up (chain sync resumes)",
                pending.version
            ),
        );
        restart.store(true, Ordering::SeqCst);
    } else {
        log_event(
            "update",
            &format!(
                "{} is staged; it will be applied on the next restart \
                 (GPN_UPDATE_RESTART=0)",
                pending.version
            ),
        );
    }
    Ok(())
}

/// Sleep `total`, waking early when shutdown is requested. Returns true when
/// the caller should stop.
fn sleep_interruptible(total: Duration, shutdown: &AtomicBool) -> bool {
    let mut left = total;
    while left > Duration::ZERO {
        if shutdown.load(Ordering::SeqCst) {
            return true;
        }
        let step = Duration::from_millis(500).min(left);
        std::thread::sleep(step);
        left = left.saturating_sub(step);
    }
    shutdown.load(Ordering::SeqCst)
}

/// The auto-update thread. Only spawned when the operator opted in.
fn spawn_auto_updater(
    cfg: Config,
    shutdown: Arc<AtomicBool>,
    restart: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    let interval = update::check_interval(Some(cfg.update_check_hours));
    std::thread::spawn(move || {
        log_event(
            "update",
            &format!(
                "auto-update ON — checking {} every {}h (restart_to_apply={})",
                cfg.update_url,
                cfg.update_check_hours,
                cfg.update_restart
            ),
        );
        // First check shortly after start, then on the interval.
        if sleep_interruptible(Duration::from_secs(45), &shutdown) {
            return;
        }
        loop {
            if update::read_pending(&cfg.update_dir()).is_none() {
                if let Err(e) = update_once(&cfg, &restart) {
                    log_event("update", &format!("check failed: {e}"));
                }
            }
            if restart.load(Ordering::SeqCst) {
                return;
            }
            if sleep_interruptible(interval, &shutdown) {
                return;
            }
        }
    })
}

/// Replace this process with the freshly staged binary.
#[cfg(unix)]
fn exec_self(exe: &Path) -> ! {
    use std::os::unix::process::CommandExt;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let err = std::process::Command::new(exe).args(&args).exec();
    // `exec` only returns on failure.
    asciieprintln!(
        "[supervisor] ! could not re-exec {} ({err}) — continuing on the current image",
        exe.display()
    );
    std::process::exit(0);
}

#[cfg(not(unix))]
fn exec_self(exe: &Path) -> ! {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match std::process::Command::new(exe).args(&args).spawn() {
        Ok(_) => std::process::exit(0),
        Err(e) => {
            asciieprintln!(
                "[supervisor] ! could not start {} ({e}) — continuing on the current image",
                exe.display()
            );
            std::process::exit(0);
        }
    }
}

/// Swap in a staged binary (if any) and re-exec into it.
///
/// Called at startup and after an auto-update restart. Only returns when there
/// is nothing to apply (or the swap was deferred by the OS).
fn apply_staged_or_reexec(cfg: &Config) {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            log_event("update", &format!("cannot resolve own path: {e} — skipping staged swap"));
            return;
        }
    };
    match update::apply_pending(&cfg.update_dir(), &exe) {
        Ok(Ok(Some(applied))) => {
            log_event(
                "update",
                &format!(
                    "{} verified and swapped in; previous binary kept at {} (rollback: move it back \
                     and restart); re-exec into the new build — short outage",
                    applied.version,
                    applied.backup.display()
                ),
            );
            exec_self(&exe);
        }
        Ok(Ok(None)) => {}
        Ok(Err(deferred)) => log_event(
            "update",
            &format!(
                "staged {} could not be swapped in while running ({}); it will be applied on the \
                 next start",
                deferred.version, deferred.reason
            ),
        ),
        Err(e) => log_event(
            "update",
            &format!("staged update REJECTED at swap time: {e} — staying on the current build"),
        ),
    }
}

/// `--setup` question: "Keep this node auto-updated? [y/N]".
///
/// Default is **no** (opt-in). The answer is written to the config file so the
/// next start honours it without any env var.
fn ask_auto_update(cfg: &Config) -> Option<bool> {
    let mut stdin = std::io::stdin().lock();
    if !std::io::stdin().is_terminal() {
        asciiprintln!(
            "  auto-update: unchanged ({}); no TTY to ask — set GPN_AUTO_UPDATE=1 or \
             `auto_update = true` in the config",
            if cfg.auto_update { "ON" } else { "off" }
        );
        return None;
    }
    asciiprintln!("");
    asciiprintln!("  Keep this node auto-updated? [y/N]");
    asciiprintln!("    yes: verify + install GPN releases automatically (signed manifest,");
    asciiprintln!("         ed25519), restarting briefly when one is published. Recommended:");
    asciiprintln!("         the network drops nodes that fall below the minimum version, and an");
    asciiprintln!("         old node cannot mine.");
    asciiprintln!("    no : you update manually. This node will REFUSE TO MINE once its");
    asciiprintln!("         version is below the published minimum (it keeps syncing).");
    asciiprintln!("  Currently: {}", if cfg.auto_update { "ON" } else { "off" });
    asciiprint!("  > ");
    let mut line = String::new();
    if stdin.read_line(&mut line).is_err() {
        return None;
    }
    let ans = line.trim().to_ascii_lowercase();
    if ans.is_empty() {
        asciiprintln!("  auto-update: unchanged ({})", if cfg.auto_update { "ON" } else { "off" });
        return None;
    }
    let enable = matches!(ans.as_str(), "y" | "yes");
    match persist_auto_update(cfg, enable) {
        Ok(path) => {
            asciiprintln!(
                "  auto-update: {} (written to {})",
                if enable { "ON" } else { "off" },
                path.display()
            );
            Some(enable)
        }
        Err(e) => {
            asciieprintln!("  ! could not write the config ({e}); set GPN_AUTO_UPDATE={} instead", if enable { "1" } else { "0" });
            None
        }
    }
}

/// Write `auto_update` into the config file (creating a minimal one when none
/// exists) so the choice survives restarts.
fn persist_auto_update(cfg: &Config, enabled: bool) -> Result<PathBuf, String> {
    let path = cfg.config_path.clone().unwrap_or_else(|| {
        let prefix = expand_tilde(&env_str("GPN_PREFIX").unwrap_or_else(|| "~/.gpn".into()));
        PathBuf::from(prefix).join("node.toml")
    });
    let body = if path.is_file() {
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?
    } else {
        let ks = gpn_agent::firstrun::default_keystore_path(&cfg.base_path);
        gpn_agent::firstrun::config_toml(
            &cfg.base_path,
            &cfg.chain,
            ks.is_file().then_some(ks.as_path()),
        )
    };
    let updated = update::set_toml_bool(&body, "auto_update", enabled);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = PathBuf::from(format!("{}.tmp-{}", path.display(), std::process::id()));
    std::fs::write(&tmp, updated).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
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
                parts.push(format!(
                    "{label}=down(not dialable, restarts {})",
                    st.restarts
                ));
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
        // Explicit URLs replace the node's built-in default; leaving the list
        // empty lets gpn-node use https://gpncoin.ai/seeds.json on its own.
        for url in &cfg.bootnodes_url {
            argv.push("--bootnodes-url".into());
            argv.push(url.clone());
        }
        if let Some(pa) = &cfg.public_addr {
            argv.push("--public-addr".into());
            argv.push(pa.clone());
        }
        // Below `min_version`: stay synced, but do not announce and (via
        // GPN_STANDBY) do not mine. An outdated node must never be able to
        // gossip itself as a peer or seal a block the network rejects.
        if cfg.standby {
            argv.push("--no-seed".into());
            argv.push("--no-discoverable".into());
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
        if cfg.standby {
            envs.push(("GPN_STANDBY".to_string(), "1".to_string()));
        }

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
        // The heartbeat reads the node's libp2p peer-id over the node RPC.
        // Pass the configured endpoint explicitly (GPN_ENDPOINT) — the child
        // otherwise falls back to ws://127.0.0.1:9944, which is wrong when the
        // node's RPC runs on a non-default port (e.g. box #2 uses 9945).
        if !cfg.endpoint.is_empty() {
            argv.push("--endpoint".into());
            argv.push(cfg.endpoint.clone());
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

/// `gpn_node --setup` — one-shot bootstrap of the REQUIRED ollama component.
///
/// Installs (if absent), starts `ollama serve` detached, waits for `/api/tags`,
/// and pulls the baseline model(s). Exit 0 when ollama is serving, otherwise
/// [`gpn_agent::ollama::EXIT_NO_OLLAMA`].
fn run_setup(cfg: &Config) -> i32 {
    asciiprintln!("gpn_node --setup — preparing the REQUIRED ollama component");
    asciiprintln!("  endpoint={}", cfg.ollama);

    let opts = ollama::SetupOpts::from_env(&cfg.ollama, true);
    asciiprintln!("  baseline={}", opts.baseline.join(", "));
    let outcome = ollama::ensure(&opts);

    asciiprintln!(
        "  attempted_install={} installed={}",
        outcome.attempted_install,
        outcome.installed
    );
    if outcome.models.is_empty() {
        asciiprintln!("  models: (none reported)");
    } else {
        asciiprintln!("  models: {}", outcome.models.join(", "));
    }

    // Opt-in auto-update: asked once, here, and persisted to the config file.
    // Asked even when ollama failed — the answer is independent of it.
    ask_auto_update(cfg);

    if outcome.serving {
        if outcome.model_ready {
            asciiprintln!("  [ok] {}", outcome.detail);
        } else {
            asciieprintln!(
                "  ! ollama is up, but not every baseline model is present: {}",
                outcome.detail
            );
        }
        asciiprintln!("  setup complete — a normal `gpn_node` start will pass the ollama gate.");
        print_setup_summary(cfg);
        0
    } else {
        asciieprintln!("\n  [x] SETUP FAILED: {}", outcome.detail);
        asciieprintln!("      Fix ollama, then re-run:  gpn_node --setup");
        asciieprintln!(
            "        Windows :  winget install Ollama.Ollama   (or scripts\\install-ollama.ps1)"
        );
        asciieprintln!("        Linux   :  scripts/install-ollama.sh");
        asciieprintln!("        Then    :  ollama serve   &&   ollama pull llama3.2:1b\n");
        ollama::EXIT_NO_OLLAMA
    }
}

/// The last thing `--setup` prints: what is configured, what is automatic, and
/// the one command to run next. The dialability sentence is the actionable one
/// from the NOT-DIALABLE notice, reduced to what a fresh install needs.
fn print_setup_summary(cfg: &Config) {
    asciiprintln!("\n  node data   : {}", cfg.base_path.display());
    asciiprintln!("  chain spec  : {}", cfg.chain);
    match gpn_agent::firstrun::ss58_of(&cfg.signer) {
        Ok(addr) => asciiprintln!("  wallet      : {addr}"),
        Err(_) => asciiprintln!("  wallet      : {} (dev URI)", cfg.signer),
    }
    asciiprintln!("  payout      : {}", cfg.account);
    asciiprintln!(
        "  public addr : {}",
        cfg.public_addr.as_deref().unwrap_or("<auto>")
    );
    asciiprintln!(
        "\n  Network reachability is automatic: configured address -> public-IP detection -> \
         UPnP/NAT-PMP port mapping (on by default). If UPnP is disabled on your router, \
         forward TCP 30333 to this machine — the node prints the exact rule if it cannot get in."
    );
    asciiprintln!(
        "\n  Updates: {}. A node below the published minimum version refuses to mine\n  \
         (it keeps syncing). Auto-update is opt-in: GPN_AUTO_UPDATE=1 or the\n  \
         `auto_update = true` config key; see docs/updates.md.",
        if cfg.auto_update { "auto-update ON" } else { "manual" }
    );
    asciiprintln!("\n  Start the node:  {}\n", exe_hint());
}

/// `./gpn_node` (Unix) / `gpn_node.exe` (Windows) for the final hint.
fn exe_hint() -> &'static str {
    if cfg!(windows) {
        "gpn_node.exe"
    } else {
        "./gpn_node"
    }
}

/// The REQUIRED-ollama gate, run on every start.
///
/// Probes `GPN_OLLAMA`; if missing, attempts the automated bootstrap (unless
/// `GPN_OLLAMA_SETUP=0`). If ollama is still unreachable and
/// `GPN_REQUIRE_OLLAMA` is on (the default), the node **refuses to start** —
/// it never silently runs miner-only.
fn enforce_ollama_gate(cfg: &Config) -> i32 {
    let require = ollama::require_ollama(env_str("GPN_REQUIRE_OLLAMA").as_deref());
    let opts = ollama::SetupOpts::from_env(&cfg.ollama, false);
    let outcome = ollama::ensure(&opts);

    match ollama::gate(require, outcome.serving) {
        ollama::GateDecision::Allow => {
            asciiprintln!("  ollama: {}", outcome.detail);
            0
        }
        ollama::GateDecision::AllowDegraded => {
            asciieprintln!(
                "\n  ! ollama NOT reachable at {} and GPN_REQUIRE_OLLAMA=0 — starting in DEGRADED mode:",
                cfg.ollama
            );
            asciieprintln!("      the node will MINE, but it CANNOT serve LLM jobs.");
            asciieprintln!(
                "      Startup will not be blocked, but this is NOT a supported production node."
            );
            asciieprintln!("      Fix: gpn_node --setup\n");
            0
        }
        ollama::GateDecision::Refuse => {
            asciieprintln!("\n  [x] REFUSING TO START: ollama is REQUIRED and is not reachable.");
            asciieprintln!("      endpoint: {}", cfg.ollama);
            asciieprintln!("      reason  : {}", outcome.detail);
            asciieprintln!(
                "      A GPN node is miner AND AI provider — a miner-only node is not valid."
            );
            asciieprintln!("      Fix it with the one-shot bootstrap:");
            asciieprintln!("        gpn_node --setup");
            asciieprintln!("      Manual install:");
            asciieprintln!(
                "        Windows : winget install Ollama.Ollama   (or scripts\\install-ollama.ps1)"
            );
            asciieprintln!("        Linux   : scripts/install-ollama.sh");
            asciieprintln!("        then    : ollama serve   &&   ollama pull llama3.2:1b");
            asciieprintln!("      Docs: docs/run-a-node.md");
            asciieprintln!("      (special cases only: GPN_REQUIRE_OLLAMA=0 starts degraded)");
            asciieprintln!("\n      node=down(no ollama)\n");
            ollama::EXIT_NO_OLLAMA
        }
    }
}

#[tokio::main]
async fn main() {
    env_logger::init();
    let argv: Vec<String> = std::env::args().collect();
    set_ascii_mode(ascii_mode_default(&argv));

    // ── Informational flags, answered BEFORE first-run/setup detection. ─────
    // `--version`/`-V` and `--help`/`-h` must never enter `Config::load()`
    // (that creates the base dir, may fetch the chain spec and can run the
    // wallet wizard). Pure output, exit 0.
    match gpn_agent::firstrun::info_request(&argv) {
        Some(gpn_agent::firstrun::InfoRequest::Version) => {
            println!("{}", gpn_agent::firstrun::version_line());
            std::process::exit(0);
        }
        Some(gpn_agent::firstrun::InfoRequest::Help) => {
            print!("{}", gpn_agent::firstrun::help_text());
            std::process::exit(0);
        }
        None => {}
    }

    let mut cfg = match Config::load() {
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
    asciiprintln!(
        "  base_path={} (node database + wallet)",
        cfg.base_path.display()
    );
    if let Some(p) = &cfg.config_path {
        asciiprintln!("  config={}", p.display());
    }
    if cfg.autostart {
        asciiprintln!(
            "  autostart=true (informational — install a service unit; see docs/run-a-node.md)"
        );
    }
    asciiprintln!(
        "  updates: auto={} every {}h restart_to_apply={} manifest={}",
        if cfg.auto_update { "ON (opt-in)" } else { "off (manual)" },
        cfg.update_check_hours,
        cfg.update_restart,
        if cfg.update_check { cfg.update_url.as_str() } else { "disabled" }
    );

    // ── Apply a staged update first: it must be swapped in before any child is
    //    spawned, so this process can be replaced cleanly (short outage). ─────
    apply_staged_or_reexec(&cfg);

    // ── `--check-updates`: answer "is this build still allowed to mine?" and
    //    exit. Read-only: fetches nothing but the manifest, installs nothing.
    if argv.iter().any(|a| a == "--check-updates") {
        let mut probe = cfg.clone();
        let standby = on_plain_thread(
            move || {
                startup_update_check(&mut probe);
                probe.standby
            },
            false,
        );
        asciiprintln!(
            "\n  result: this node ({}) {}",
            env!("CARGO_PKG_VERSION"),
            if standby {
                "is BELOW the minimum version — it would SYNC but NOT MINE"
            } else {
                "is supported — it would mine"
            }
        );
        std::process::exit(0);
    }

    // ── `--setup`: explicit one-shot bootstrap of the REQUIRED ollama part. ──
    // Run on a plain thread: the ollama probe/pull use reqwest's *blocking*
    // client, which must not be created/dropped inside the async runtime.
    if argv.iter().any(|a| a == "--setup") {
        let cfg = cfg.clone();
        let code = std::thread::spawn(move || run_setup(&cfg))
            .join()
            .unwrap_or(ollama::EXIT_NO_OLLAMA);
        std::process::exit(code);
    }

    // ── Minimum-version cutoff: one signed-manifest check at startup. ────────
    // Independent of the auto-update OPT-IN: a manual updater still has to be
    // told that its build is too old to mine. Runs on a plain thread: the
    // blocking HTTP client must not be created inside the async runtime.
    cfg.standby = {
        let mut probe = cfg.clone();
        on_plain_thread(
            move || {
                startup_update_check(&mut probe);
                probe.standby
            },
            false,
        )
    };

    // ── Ollama is a REQUIRED first-class component (not optional): probe, try
    //    the automated bootstrap, then HARD-FAIL if it is still unreachable.
    //    A GPN node is always miner + AI provider; miner-only is not valid. ──
    let gate_code = {
        let cfg = cfg.clone();
        std::thread::spawn(move || enforce_ollama_gate(&cfg))
            .join()
            .unwrap_or(ollama::EXIT_NO_OLLAMA)
    };
    if gate_code != 0 {
        std::process::exit(gate_code);
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

    // Auto-update thread — only when the operator opted in.
    let restart = Arc::new(AtomicBool::new(false));
    let updater_handle = if cfg.auto_update {
        Some(spawn_auto_updater(
            cfg.clone(),
            shutdown.clone(),
            restart.clone(),
        ))
    } else {
        None
    };

    // Wait for Ctrl-C / SIGTERM — or for a verified update to ask for a restart.
    let restart_wait = restart.clone();
    tokio::select! {
        _ = wait_for_signal() => {
            asciiprintln!("\n[supervisor] shutdown requested — stopping children…");
        }
        _ = async move {
            while !restart_wait.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        } => {
            asciiprintln!("\n[supervisor] update: applying a staged release — stopping children…");
        }
    }
    shutdown.store(true, Ordering::SeqCst);

    for h in handles {
        let _ = h.join();
    }
    let _ = status_handle.join();
    let _ = watchdog_handle.join();
    if let Some(h) = updater_handle {
        let _ = h.join();
    }

    // If an update asked for the restart, swap the binary and re-exec. This is
    // the only place the live binary is ever replaced, and only after the
    // staged file was hash-verified again.
    if restart.load(Ordering::SeqCst) {
        apply_staged_or_reexec(&cfg);
        log_event(
            "update",
            "staged update could not be applied now — it will be applied on the next start",
        );
    }
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

    /// A fully-populated config for tests that must not touch the real
    /// environment (no `Config::load`, no network, no wallet).
    fn test_config() -> Config {
        Config {
            chain: "dev".into(),
            signer: "//Alice".into(),
            account: "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY".into(),
            endpoint: "ws://127.0.0.1:9944".into(),
            base_path: PathBuf::from("/tmp/gpn-test-base"),
            rpc_port: 9944,
            node_key: None,
            bootnodes: vec![],
            bootnodes_url: vec![],
            public_addr: None,
            author: None,
            node_args: vec![],
            provider_endpoint: None,
            assign_token: String::new(),
            ollama: "http://127.0.0.1:11434".into(),
            listen: "0.0.0.0:8093".into(),
            gflops: 0,
            capacity: 1,
            poll_ms: 300,
            diffusion: false,
            diffusion_poll_ms: 2000,
            ml: false,
            ml_poll_ms: 2000,
            finetune: false,
            finetune_poll_ms: 4000,
            bin_dir: PathBuf::from("/tmp/gpn-test-bin"),
            status_secs: 15,
            autostart: false,
            auto_update: false,
            update_url: update::MANIFEST_URL.to_string(),
            update_check_hours: 6,
            update_restart: true,
            update_check: true,
            min_version_enforce: true,
            standby: false,
            config_path: None,
        }
    }

    fn node_spec(cfg: &Config) -> ChildSpec {
        build_children(cfg)
            .into_iter()
            .find(|s| s.label == "node")
            .expect("a node child is always built")
    }

    /// The cutoff must actually change what the child is launched with: an
    /// out-of-date node keeps syncing but never announces itself, and tells the
    /// node binary (via `GPN_STANDBY`) not to mine.
    #[test]
    fn standby_node_neither_announces_nor_mines() {
        let mut cfg = test_config();
        cfg.standby = true;
        let spec = node_spec(&cfg);
        assert!(
            spec.argv.iter().any(|a| a == "--no-seed"),
            "standby must not advertise as a seed: {:?}",
            spec.argv
        );
        assert!(
            spec.argv.iter().any(|a| a == "--no-discoverable"),
            "standby must not accept/publish an endpoint: {:?}",
            spec.argv
        );
        assert!(
            spec.envs
                .iter()
                .any(|(k, v)| k == "GPN_STANDBY" && v == "1"),
            "the node must be told to skip the miner: {:?}",
            spec.envs
        );
        // It still syncs: nothing about --sync / --chain is dropped.
        assert!(spec.argv.iter().any(|a| a == "--sync"));
        assert!(spec.argv.iter().any(|a| a == "dev"));
    }

    #[test]
    fn a_supported_node_announces_and_mines() {
        let cfg = test_config();
        assert!(!cfg.standby);
        let spec = node_spec(&cfg);
        assert!(!spec.argv.iter().any(|a| a == "--no-seed"));
        assert!(!spec.argv.iter().any(|a| a == "--no-discoverable"));
        assert!(!spec.envs.iter().any(|(k, _)| k == "GPN_STANDBY"));
    }

    /// Auto-update is opt-in: a config that does not ask for it must produce an
    /// updater that is never spawned.
    #[test]
    fn auto_update_defaults_to_off() {
        let cfg = test_config();
        assert!(!cfg.auto_update);
        assert!(!update::auto_update_enabled(None));
        assert!(update::auto_update_enabled(Some("1")));
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
