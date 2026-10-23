//! First-run setup: zero-question defaults for a GPN node.
//!
//! Installing a node should be **download → run → answer one question**
//! (generate or import a wallet). Everything else is chosen automatically:
//!
//! * **base path** — `GPN_BASE_PATH` (or config `base_path`), else the OS
//!   default (`~/.gpn/node` on Unix, `C:\gpn\data` on Windows), created on
//!   first use. It holds the node database **and** the wallet keystore.
//! * **chain spec** — `GPN_CHAIN` (or config `chain`), else
//!   `<base>/testnet-spec.json`, else it is **fetched** from the public access
//!   point (`GET /genesis-spec`) and saved there. No prompt; one log line.
//! * **wallet** — `GPN_SIGNER` → config `signer` → `GPN_SIGNER_FILE` →
//!   config `signer_file` → `<base>/keystore.json`. If none exists and stdin is
//!   a TTY, the wizard generates/imports one. If stdin is **not** a TTY
//!   (systemd, a container, CI) it refuses with an actionable message instead
//!   of hanging on a prompt.
//!
//! The keystore is a small JSON document (`address`/`crypto`/`mnemonic`,
//! the same shape `gpn_wallet` reads) written **owner-only**: mode `0600` on
//! Unix, and an explicit user-only ACL on Windows (`icacls /inheritance:r`).
//! The secret is **never** written to the config file or the repo.
//!
//! Dialability is untouched here: the node still resolves
//! explicit `GPN_PUBLIC_ADDR` → public-IP detection → UPnP/NAT-PMP (on by
//! default). A successful UPnP mapping needs no user configuration at all.

use sp_core::crypto::{Pair as _, Ss58Codec};
use sp_core::sr25519;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

/// Public access point endpoint that serves the canonical chain spec.
/// `gpn_scheduler` exposes it at `GET /genesis-spec`.
pub const GENESIS_SPEC_URL: &str = "http://gpn-testnet.duckdns.org:8092/genesis-spec";

/// File name of the pinned testnet spec inside the base path.
pub const DEFAULT_SPEC_FILE: &str = "testnet-spec.json";

/// File name of the wallet keystore inside the base path.
pub const KEYSTORE_FILE: &str = "keystore.json";

/// Message shown when a service (non-TTY) starts without a wallet.
pub const NO_WALLET_NON_TTY: &str =
    "no wallet is configured and stdin is not a TTY — run `gpn_node --setup` once \
     interactively (generate or import a wallet), then start the service again. \
     Alternatively set GPN_SIGNER (mnemonic/seed) or GPN_SIGNER_FILE (keystore path).";

// ─────────────────────────────────────────────────────────────────────────────
// CLI info — answered BEFORE any setup detection
// ─────────────────────────────────────────────────────────────────────────────

/// An informational flag that must be answered immediately, before the
/// first-run detection (and therefore before any network fetch, config write
/// or wallet prompt).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfoRequest {
    Help,
    Version,
}

/// Detect `-h`/`--help` and `-V`/`--version` in a full `argv` (index 0 is the
/// program name and is skipped).
///
/// This is a **pure** function on purpose: the caller can answer it without
/// touching the filesystem, the network, or the wallet wizard. The first
/// recognised flag wins, so `gpn_node --setup --help` prints help.
pub fn info_request(argv: &[String]) -> Option<InfoRequest> {
    for a in argv.iter().skip(1) {
        match a.as_str() {
            "-h" | "--help" => return Some(InfoRequest::Help),
            "-V" | "--version" => return Some(InfoRequest::Version),
            _ => {}
        }
    }
    None
}

/// The machine-readable build stamp, injected by `build.rs`:
/// `gpn-build-v1 version=0.2.0 sha=d606264 spec=41`.
///
/// It is a plain string literal in the binary, which is how
/// `scripts/gen-release-manifest.py` reads the **version and SHA an artifact was
/// actually built from** — including for a Windows `.exe` that cannot be
/// executed on the release host. Do not change the `gpn-build-v1 version=`
/// prefix without updating that script.
pub fn build_id() -> &'static str {
    option_env!("GPN_BUILD_ID").unwrap_or("gpn-build-v1 version=unknown sha=unknown")
}

/// The git commit this binary was built from, injected by `build.rs`.
///
/// `unknown` when the crate is built outside a git work tree (or git is not
/// installed). Callers must tolerate that; nothing depends on the value, it
/// exists so an operator can tell two same-version builds apart
/// (`docs/updates.md` → "Version numbering").
///
/// Read out of `build_id()` rather than a separate `option_env!` so the stamp
/// is guaranteed to be referenced by the program and therefore to survive into
/// the linked binary.
pub fn git_sha() -> &'static str {
    build_id()
        .split_whitespace()
        .find_map(|field| field.strip_prefix("sha="))
        .unwrap_or("unknown")
}

/// The one-line `--version` output, e.g. `gpn_node 0.2.0 (spec 41, d606264)`.
///
/// The runtime `spec_version` is injected at build time from
/// `gpn-node/runtime/src/lib.rs` and the git SHA from `git rev-parse` (see
/// `build.rs`); when a piece is unavailable it is omitted, so the line degrades
/// to `gpn_node 0.2.0 (spec 41)`, `gpn_node 0.2.0 (d606264)` or
/// `gpn_node 0.2.0`.
pub fn version_line() -> String {
    version_line_with(
        env!("CARGO_PKG_VERSION"),
        option_env!("GPN_RUNTIME_SPEC_VERSION"),
        Some(git_sha()),
    )
}

/// `version_line` with its inputs supplied — the formatting rules, testable
/// without a particular build environment.
///
/// * the version is always printed;
/// * `spec <n>` is printed when the runtime spec version is known;
/// * the short git SHA is printed when it is known (never `unknown`);
/// * details are wrapped in one `(...)`, comma-separated, ASCII only.
pub fn version_line_with(version: &str, spec: Option<&str>, sha: Option<&str>) -> String {
    let mut details: Vec<String> = Vec::new();
    if let Some(spec) = spec.map(str::trim).filter(|s| !s.is_empty()) {
        details.push(format!("spec {spec}"));
    }
    if let Some(sha) = sha
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "unknown")
    {
        details.push(sha.to_string());
    }
    if details.is_empty() {
        format!("gpn_node {version}")
    } else {
        format!("gpn_node {version} ({})", details.join(", "))
    }
}

/// The `--help` text: usage, the two one-shot flags, and the key env vars.
///
/// ASCII-only so it renders the same on every console.
pub fn help_text() -> String {
    concat!(
        "gpn_node - the single-node supervisor (miner + AI provider, one command).\n",
        "\n",
        "Usage:\n",
        "  gpn_node               start (and supervise) the node\n",
        "  gpn_node --gui         start, and serve the local dashboard on\n",
        "                         127.0.0.1:8099 (idle switch + status)\n",
        "  gpn_node idle <min>    pause mining + AI jobs for <min> minutes (max 360)\n",
        "                         the node keeps syncing/peering; auto-resumes\n",
        "  gpn_node resume        end an idle window now\n",
        "  gpn_node --setup       one-time bootstrap: wallet + ollama (interactive)\n",
        "  gpn_node --check-updates  is this build still allowed to mine? (read-only)\n",
        "  gpn_node --version     print the version and exit\n",
        "  gpn_node --help        print this help and exit\n",
        "\n",
        "Everything else is automatic on first run (chain spec, data dir, peers,\n",
        "public address). Only a wallet is asked for, once, by `--setup`.\n",
        "\n",
        "Environment (all optional; a TOML config can set the same values):\n",
        "  GPN_BASE_PATH         node data dir - chain DB + wallet\n",
        "                        (default ~/.gpn/node; Windows C:\\gpn\\data)\n",
        "  GPN_CHAIN             chain spec: testnet | mainnet | dev | <path>\n",
        "  GPN_SIGNER            signer URI / seed / mnemonic\n",
        "  GPN_SIGNER_FILE       sr25519 JSON keystore (owner-only, 0600)\n",
        "  GPN_PUBLIC_ADDR       public multiaddr, e.g. /dns4/host/tcp/30333\n",
        "  GPN_UPNP              auto (default) | off\n",
        "  GPN_SEEDS_URL         published seed-list URL(s)\n",
        "                        (alias: GPN_BOOTNODES_URL)\n",
        "  GPN_REQUIRE_BACKBONE  1 = refuse to run unless dialable (default 0)\n",
        "  GPN_SEED              1 = may be published as a seed (default 1)\n",
        "  GPN_DISCOVERABLE      0 = private: no announce, no inbound (implies GPN_SEED=0)\n",
        "  GPN_OLLAMA            ollama base URL (default http://127.0.0.1:11434)\n",
        "  GPN_AUTO_UPDATE       1 = install signed GPN releases automatically (opt-in,\n",
        "                        default off; asked once by `--setup`)\n",
        "  GPN_UPDATE_URL        signed release manifest URL\n",
        "                        (default https://gpncoin.ai/releases.json)\n",
        "  GPN_UPDATE_CHECK_HOURS  hours between auto-update checks (default 6;\n",
        "                        each node checks at its own random phase, +/-5-10% jitter)\n",
        "  GPN_UPDATE_SPREAD_SECS  max seconds to wait before downloading a found\n",
        "                        update, so the fleet does not stampede the release\n",
        "                        host (default 1200 = 20 min; 0 disables)\n",
        "  GPN_UPDATE_RESTART    0 = stage a new release but apply it at the next start\n",
        "  GPN_MIN_VERSION_ENFORCE  0 = mine even when below the published minimum\n",
        "                        (emergency only; NOT supported)\n",
        "  GPN_GUI               1 = serve the local dashboard (same as --gui)\n",
        "  GPN_GUI_LISTEN        dashboard bind address (default 127.0.0.1:8099;\n",
        "                        loopback only - any other address is refused)\n",
        "  GPN_IDLE_FORCE        1 = allow idle even with no peers (dev chains only)\n",
        "  GPN_IDLE_FILE         idle state file (default ~/.gpn/idle.json)\n",
        "\n",
        "Docs: docs/run-a-node.md\n",
    )
    .to_string()
}

// ─────────────────────────────────────────────────────────────────────────────
// Base path
// ─────────────────────────────────────────────────────────────────────────────

/// The default node data directory: `~/.gpn/node` (Unix), `C:\gpn\data` (Win).
///
/// It holds the chain database **and** the wallet keystore, and is the anchor
/// for the auto-fetched chain spec, so a second run needs no configuration.
pub fn default_base_path() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(r"C:\gpn\data")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(expand_tilde("~/.gpn/node"))
    }
}

/// Expand a leading `~` to `$HOME` (no-op when `$HOME` is unset).
pub fn expand_tilde(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    p.to_string()
}

/// The path of the keystore that lives in the base dir.
pub fn default_keystore_path(base: &Path) -> PathBuf {
    base.join(KEYSTORE_FILE)
}

// ─────────────────────────────────────────────────────────────────────────────
// Chain spec
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainSource {
    /// `GPN_CHAIN` / config `chain` — left exactly as the operator wrote it.
    Explicit,
    /// The pinned spec already existed in the base dir.
    SpecFile,
    /// Downloaded from the public access point and saved into the base dir.
    Fetched,
    /// Nothing above worked — the node's compiled-in `testnet` spec is used.
    Builtin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainResolution {
    /// Value to pass to `gpn-node --chain`.
    pub chain: String,
    pub source: ChainSource,
    /// The spec file on disk, when the resolution is path-based.
    pub spec_path: Option<PathBuf>,
}

/// Resolve the chain spec, fetching it when the default file is missing.
///
/// Order: explicit `chain` → `<base>/testnet-spec.json` → fetch → `testnet`.
/// An explicit path to a **missing** `testnet-spec.json` also triggers a fetch;
/// a missing custom `.json` (someone's own spec) is left alone so the node
/// reports it.
pub fn resolve_chain(explicit: Option<String>, base: &Path) -> ChainResolution {
    resolve_chain_with(explicit, base, fetch_spec_guarded)
}

/// Testable core of [`resolve_chain`] — the fetcher is injected.
pub fn resolve_chain_with<F>(explicit: Option<String>, base: &Path, fetch: F) -> ChainResolution
where
    F: FnOnce(&str) -> Result<String, String>,
{
    let spec_in_base = base.join(DEFAULT_SPEC_FILE);

    if let Some(raw) = explicit.filter(|s| !s.trim().is_empty()) {
        let value = raw.trim().to_string();
        if !value.to_ascii_lowercase().ends_with(".json") {
            return ChainResolution {
                chain: value,
                source: ChainSource::Explicit,
                spec_path: None,
            };
        }
        let path = PathBuf::from(expand_tilde(&value));
        if path.is_file() {
            return ChainResolution {
                chain: path.to_string_lossy().into_owned(),
                source: ChainSource::Explicit,
                spec_path: Some(path),
            };
        }
        // A missing `testnet-spec.json` is the canonical name → re-fetch it.
        let is_default_name = path.file_name().and_then(|s| s.to_str()) == Some(DEFAULT_SPEC_FILE);
        if is_default_name {
            if let Some(p) = try_fetch_spec(&spec_in_base, fetch) {
                return ChainResolution {
                    chain: p.to_string_lossy().into_owned(),
                    source: ChainSource::Fetched,
                    spec_path: Some(p),
                };
            }
        }
        return ChainResolution {
            chain: path.to_string_lossy().into_owned(),
            source: ChainSource::Explicit,
            spec_path: Some(path),
        };
    }

    if spec_in_base.is_file() {
        return ChainResolution {
            chain: spec_in_base.to_string_lossy().into_owned(),
            source: ChainSource::SpecFile,
            spec_path: Some(spec_in_base),
        };
    }

    if let Some(p) = try_fetch_spec(&spec_in_base, fetch) {
        return ChainResolution {
            chain: p.to_string_lossy().into_owned(),
            source: ChainSource::Fetched,
            spec_path: Some(p),
        };
    }

    println!(
        "[gpn_node] chain spec: could not fetch {GENESIS_SPEC_URL} — falling back to the \
         built-in `testnet` spec (the node will still start; set GPN_CHAIN to a local spec \
         file if the genesis hash must be pinned)"
    );
    ChainResolution {
        chain: "testnet".to_string(),
        source: ChainSource::Builtin,
        spec_path: None,
    }
}

/// Fetch and persist the spec; `None` on any failure (already logged).
fn try_fetch_spec<F>(dest: &Path, fetch: F) -> Option<PathBuf>
where
    F: FnOnce(&str) -> Result<String, String>,
{
    let url = std::env::var("GPN_SPEC_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| GENESIS_SPEC_URL.to_string());
    match fetch(&url) {
        Ok(body) => match write_spec(dest, &body) {
            Ok(()) => {
                println!(
                    "[gpn_node] chain spec: {} -> {} (auto)",
                    url,
                    dest.display()
                );
                Some(dest.to_path_buf())
            }
            Err(e) => {
                eprintln!("[gpn_node] chain spec: fetched {url} but could not save it: {e}");
                None
            }
        },
        Err(e) => {
            eprintln!("[gpn_node] chain spec: fetch {url} failed: {e}");
            None
        }
    }
}

/// Validate + atomically write a chain-spec document.
///
/// The validation matters: an access point behind a captive portal or a proxy
/// happily returns 200 for an HTML error page. A "spec" without a `genesis`
/// key would make the node fail with a confusing parse error later.
pub fn write_spec(dest: &Path, body: &str) -> Result<(), String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("not valid JSON: {e}"))?;
    if v.get("genesis").is_none() && v.get("genesisRaw").is_none() {
        return Err("JSON has no `genesis` field — not a chain spec".to_string());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    // Ensure what we are about to hand to `--chain` is a regular JSON file.
    let tmp = dest.with_extension("json.tmp");
    std::fs::write(&tmp, body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    Ok(())
}

/// Download a chain spec over HTTP(S). Runs on a worker thread so it is safe to
/// call from inside a Tokio runtime (reqwest's *blocking* client builds its own
/// runtime and must not be constructed on an async executor thread).
pub fn fetch_spec(url: &str) -> Result<String, String> {
    let url = url.to_string();
    std::thread::spawn(move || -> Result<String, String> {
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(8))
            .timeout(std::time::Duration::from_secs(45))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client
            .get(&url)
            .send()
            .map_err(|e| format!("{e}"))?
            .error_for_status()
            .map_err(|e| format!("HTTP {e}"))?;
        resp.text().map_err(|e| e.to_string())
    })
    .join()
    .unwrap_or_else(|_| Err("fetch thread panicked".to_string()))
}

fn fetch_spec_guarded(url: &str) -> Result<String, String> {
    fetch_spec(url)
}

// ─────────────────────────────────────────────────────────────────────────────
// Wallet keystore
// ─────────────────────────────────────────────────────────────────────────────

/// The on-disk keystore (same JSON shape `gpn_wallet` reads and writes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keystore {
    pub address: String,
    pub crypto: String,
    pub mnemonic: String,
}

impl Keystore {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&serde_json::json!({
            "address": self.address,
            "crypto": self.crypto,
            "mnemonic": self.mnemonic,
        }))
        .unwrap_or_else(|_| "{}".to_string())
    }

    pub fn from_json(raw: &str) -> Result<Keystore, String> {
        let v: serde_json::Value =
            serde_json::from_str(raw).map_err(|e| format!("invalid keystore JSON: {e}"))?;
        let mnemonic = v
            .get("mnemonic")
            .and_then(|m| m.as_str())
            .ok_or("keystore missing `mnemonic`")?
            .to_string();
        Ok(Keystore {
            address: v
                .get("address")
                .and_then(|a| a.as_str())
                .unwrap_or_default()
                .to_string(),
            crypto: v
                .get("crypto")
                .and_then(|c| c.as_str())
                .unwrap_or("sr25519")
                .to_string(),
            mnemonic,
        })
    }

    /// The signer URI for the provider/node children.
    pub fn secret(&self) -> &str {
        &self.mnemonic
    }

    /// True when the stored address matches the secret (guards a hand-edited
    /// keystore from paying out to the wrong account).
    pub fn address_is_consistent(&self) -> bool {
        match ss58_of(&self.mnemonic) {
            Ok(a) => self.address.is_empty() || a == self.address,
            Err(_) => false,
        }
    }
}

/// Validate a mnemonic or `0x` seed and return its SS58 address.
pub fn ss58_of(secret: &str) -> Result<String, String> {
    let pair = pair_from_secret(secret)?;
    Ok(pair.public().to_ss58check())
}

/// Like [`ss58_of`], but also accepts **dev shortcuts** (`//Bob`, `//Alice//stash`)
/// and other SURI forms, because a testnet box is launched with `//Bob`.
///
/// `pair_from_secret` deliberately rejects those (a dev key must never hold real
/// earnings), which made every *derived* use of the signer fail for such a box:
/// `gpn_node` could not fill in its payout/account, so it started the heartbeat
/// with **no `--address`**, the heartbeat could not read
/// `GPUAttestation::Attestations`, and `/status` advertised `gflops: 0` on a box
/// that had a real attestation (queue #65). Never use this to *create* a wallet —
/// only to name an account that is already configured.
pub fn ss58_of_uri(uri: &str) -> Result<String, String> {
    let uri = uri.trim();
    // `sr25519::Pair::from_string("")` is deprecated and returns a default pair
    // rather than an error — never let an empty signer name an account.
    if uri.is_empty() {
        return Err("empty signer".to_string());
    }
    if let Ok(a) = ss58_of(uri) {
        return Ok(a);
    }
    let pair =
        sr25519::Pair::from_string(uri, None).map_err(|e| format!("invalid signer URI: {e:?}"))?;
    Ok(pair.public().to_ss58check())
}

/// Turn a user-supplied secret into an sr25519 pair.
///
/// Accepted: a BIP-39 mnemonic (12/15/18/21/24 words) or a `0x`-prefixed
/// 32-byte hex seed. Dev shortcuts (`//Alice`) are **not** accepted by the
/// wizard — they are public knowledge and must never hold earnings.
pub fn pair_from_secret(secret: &str) -> Result<sr25519::Pair, String> {
    let secret = secret.trim();
    if secret.is_empty() {
        return Err("empty secret".to_string());
    }
    if let Some(hexpart) = secret
        .strip_prefix("0x")
        .or_else(|| secret.strip_prefix("0X"))
    {
        let bytes = hex::decode(hexpart).map_err(|e| format!("invalid hex seed: {e}"))?;
        if bytes.len() != 32 {
            return Err(format!(
                "seed must be 32 bytes (64 hex chars); got {}",
                bytes.len()
            ));
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&bytes);
        return sr25519::Pair::from_seed_slice(&seed)
            .map_err(|e| format!("invalid sr25519 seed: {e:?}"));
    }
    let words = secret.split_whitespace().count();
    if !matches!(words, 12 | 15 | 18 | 21 | 24) {
        return Err(format!(
            "expected a 12/15/18/21/24-word mnemonic or a 0x-prefixed 32-byte seed \
             (got {words} word(s))"
        ));
    }
    sr25519::Pair::from_string(secret, None).map_err(|e| format!("invalid mnemonic: {e:?}"))
}

/// Create a fresh wallet: returns the keystore plus the one-time phrase.
pub fn generate_keystore() -> Keystore {
    let (pair, phrase, _seed) = sr25519::Pair::generate_with_phrase(None);
    Keystore {
        address: pair.public().to_ss58check(),
        crypto: "sr25519".to_string(),
        mnemonic: phrase,
    }
}

/// Read a keystore and check it is usable. Warns (does not fail) when the file
/// is group/world readable on Unix — the operator can still fix it with chmod.
pub fn read_keystore(path: &Path) -> Result<Keystore, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read keystore '{}': {e}", path.display()))?;
    let ks = Keystore::from_json(&raw)?;
    if !ks.address_is_consistent() {
        return Err(format!(
            "keystore '{}' is inconsistent: address {} does not match its secret",
            path.display(),
            ks.address
        ));
    }
    warn_if_permissive(path);
    Ok(ks)
}

/// Resolve the signer secret from a keystore path.
pub fn secret_from_keystore(path: &Path) -> Result<String, String> {
    Ok(read_keystore(path)?.mnemonic)
}

fn warn_if_permissive(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mode = meta.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                eprintln!(
                    "[gpn_node] ! keystore {} is mode {:04o} — restrict it: chmod 600 {}",
                    path.display(),
                    mode,
                    path.display()
                );
            }
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Write the keystore owner-only.
///
/// Unix: created with mode `0600` (and re-asserted after the write).
/// Windows: inheritance is removed and only the current user is granted access
/// via `icacls` (no new dependency; `%USERNAME%` is always set on Windows).
pub fn write_keystore(path: &Path, ks: &Keystore) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    write_private(path, ks.to_json().as_bytes())
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    f.write_all(bytes)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    f.flush().ok();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("cannot chmod 600 {}: {e}", path.display()))?;
    Ok(())
}

#[cfg(windows)]
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    // Drop inherited ACEs first (a file under %USERPROFILE% inherits the
    // profile ACL, which may include Administrators/Users). Then grant only
    // the current user. Done before the secret is written.
    if let Ok(user) = std::env::var("USERNAME") {
        if !user.trim().is_empty() {
            let _ = Command::new("icacls")
                .arg(path)
                .arg("/inheritance:r")
                .arg("/grant:r")
                .arg(format!("{user}:F"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
    f.write_all(bytes)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    f.flush().ok();
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Wizard
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalletChoice {
    Generate,
    Import,
}

/// Parse the menu answer: empty/`1`/`g`/`generate` → Generate; `2`/`i`/`import`.
pub fn parse_choice(input: &str) -> Option<WalletChoice> {
    match input.trim().to_ascii_lowercase().as_str() {
        "" | "1" | "g" | "gen" | "generate" | "n" | "new" => Some(WalletChoice::Generate),
        "2" | "i" | "imp" | "import" => Some(WalletChoice::Import),
        _ => None,
    }
}

/// Parse a yes/no answer, `default` on an empty line.
pub fn parse_yes_no(input: &str, default: bool) -> Option<bool> {
    match input.trim().to_ascii_lowercase().as_str() {
        "" => Some(default),
        "y" | "yes" => Some(true),
        "n" | "no" => Some(false),
        _ => None,
    }
}

/// Refuse cleanly when there is no wallet and no terminal to ask on.
pub fn ensure_interactive(is_tty: bool) -> Result<(), String> {
    if is_tty {
        Ok(())
    } else {
        Err(NO_WALLET_NON_TTY.to_string())
    }
}

/// Ask for a mnemonic/seed and validate it. Retries up to 3 times; `None` on
/// EOF (Ctrl-D) so the caller can abort without a panic.
/// Suppress terminal echo while a secret is typed (Unix TTYs only).
///
/// A mnemonic is read from the terminal at most once per install, but it is
/// still a secret: it should not sit in the scrollback. On Windows the console
/// echo is left alone (documented in `docs/run-a-node.md`).
#[cfg(unix)]
struct EchoGuard {
    restore: bool,
}

#[cfg(unix)]
impl EchoGuard {
    fn new() -> Self {
        let tty = std::io::stdin().is_terminal();
        if tty {
            let _ = std::process::Command::new("stty").arg("-echo").status();
        }
        EchoGuard { restore: tty }
    }
}

#[cfg(unix)]
impl Drop for EchoGuard {
    fn drop(&mut self) {
        if self.restore {
            let _ = std::process::Command::new("stty").arg("echo").status();
        }
    }
}

#[cfg(not(unix))]
struct EchoGuard;

#[cfg(not(unix))]
impl EchoGuard {
    fn new() -> Self {
        EchoGuard
    }
}

fn read_imported_secret<R: BufRead, W: Write>(
    input: &mut R,
    out: &mut W,
) -> Result<Option<Keystore>, String> {
    for attempt in 0..3 {
        write!(out, "  Mnemonic (12/24 words) or 0x seed: ").map_err(|e| e.to_string())?;
        out.flush().ok();
        let mut line = String::new();
        let guard = EchoGuard::new();
        let n = input.read_line(&mut line).map_err(|e| e.to_string())?;
        drop(guard);
        if n > 0 {
            // Echo was off, so the user's Enter did not move the cursor.
            if std::io::stdin().is_terminal() {
                writeln!(out).ok();
            }
        }
        if n == 0 {
            return Ok(None);
        }
        match pair_from_secret(&line) {
            Ok(pair) => {
                return Ok(Some(Keystore {
                    address: pair.public().to_ss58check(),
                    crypto: "sr25519".to_string(),
                    mnemonic: line.trim().to_string(),
                }))
            }
            Err(e) => {
                writeln!(out, "  [x] {e}").map_err(|e| e.to_string())?;
                if attempt == 2 {
                    return Err("no valid mnemonic/seed after 3 attempts".to_string());
                }
                writeln!(out, "  Try again.").map_err(|e| e.to_string())?;
            }
        }
    }
    Err("no valid mnemonic/seed".to_string())
}

fn print_mnemonic_once<W: Write>(out: &mut W, ks: &Keystore) -> Result<(), String> {
    let bar = "=".repeat(72);
    writeln!(out, "\n{bar}").map_err(|e| e.to_string())?;
    writeln!(out, "  WRITE THIS DOWN — IT CONTROLS YOUR EARNINGS").map_err(|e| e.to_string())?;
    writeln!(
        out,
        "  It cannot be recovered. Anyone who has it can take your funds."
    )
    .map_err(|e| e.to_string())?;
    writeln!(out, "{bar}").map_err(|e| e.to_string())?;
    writeln!(
        out,
        "\n  secret phrase ({} words):",
        ks.mnemonic.split_whitespace().count()
    )
    .map_err(|e| e.to_string())?;
    writeln!(out, "\n    {}\n", ks.mnemonic).map_err(|e| e.to_string())?;
    writeln!(out, "{bar}\n").map_err(|e| e.to_string())?;
    Ok(())
}

/// Run the interactive wallet wizard.
///
/// * `path` — where the keystore is written (`<base>/keystore.json` by default)
/// * existing keystore → asks whether to keep it (default **yes**)
///
/// Returns the keystore that is now on disk.
pub fn wallet_wizard<R: BufRead, W: Write>(
    input: &mut R,
    out: &mut W,
    path: &Path,
) -> Result<Keystore, String> {
    writeln!(out, "\nSet up your wallet").map_err(|e| e.to_string())?;
    writeln!(
        out,
        "Your wallet is your node identity and receives all mining/job earnings."
    )
    .map_err(|e| e.to_string())?;

    if path.is_file() {
        match read_keystore(path) {
            Ok(ks) => {
                writeln!(out, "  existing wallet: {}", ks.address).map_err(|e| e.to_string())?;
                write!(out, "  Keep it? [Y/n]: ").map_err(|e| e.to_string())?;
                out.flush().ok();
                let mut line = String::new();
                let n = input.read_line(&mut line).map_err(|e| e.to_string())?;
                if n == 0 {
                    return Ok(ks);
                }
                if parse_yes_no(&line, true) != Some(false) {
                    return Ok(ks);
                }
            }
            Err(e) => {
                writeln!(
                    out,
                    "  ! existing keystore at {} is unusable: {e}",
                    path.display()
                )
                .map_err(|e| e.to_string())?;
                writeln!(out, "  A new wallet will replace it.").map_err(|e| e.to_string())?;
            }
        }
    }

    let mut choice = None;
    for _ in 0..3 {
        writeln!(out, "  1) Generate a new wallet (recommended)").map_err(|e| e.to_string())?;
        writeln!(out, "  2) Import an existing wallet (mnemonic or 0x seed)")
            .map_err(|e| e.to_string())?;
        write!(out, "Choose [1]: ").map_err(|e| e.to_string())?;
        out.flush().ok();
        let mut line = String::new();
        let n = input.read_line(&mut line).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("setup cancelled (no input)".to_string());
        }
        if let Some(c) = parse_choice(&line) {
            choice = Some(c);
            break;
        }
        writeln!(out, "  [x] enter 1 or 2").map_err(|e| e.to_string())?;
    }
    let choice = choice.ok_or("no valid choice after 3 attempts")?;

    let ks = match choice {
        WalletChoice::Generate => {
            let ks = generate_keystore();
            print_mnemonic_once(out, &ks)?;
            ks
        }
        WalletChoice::Import => match read_imported_secret(input, out)? {
            Some(ks) => ks,
            None => return Err("setup cancelled (no input)".to_string()),
        },
    };

    write_keystore(path, &ks)?;
    writeln!(out, "  address (SS58): {}", ks.address).map_err(|e| e.to_string())?;
    writeln!(
        out,
        "  keystore: {}  (owner-only: 0600 / user ACL)",
        path.display()
    )
    .map_err(|e| e.to_string())?;
    writeln!(
        out,
        "  Check your balance / send GPN:  gpn_wallet balance <endpoint> {}",
        ks.address
    )
    .map_err(|e| e.to_string())?;
    Ok(ks)
}

// ─────────────────────────────────────────────────────────────────────────────
// Config file
// ─────────────────────────────────────────────────────────────────────────────

/// Escape a value for a TOML basic string (Windows paths contain `\`).
pub fn toml_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Render the zero-config second-run file. **Contains no secrets** — only the
/// keystore *path* (omitted when no keystore exists, e.g. a dev chain).
pub fn config_toml(base: &Path, chain: &str, keystore: Option<&Path>) -> String {
    let signer_line = match keystore {
        Some(k) => format!(
            "signer_file = \"{}\"   # sr25519 keystore (owner-only)\n",
            toml_escape(&k.to_string_lossy())
        ),
        None => "# signer_file = \"<path>\"   # sr25519 keystore; created by `gpn_node --setup`\n"
            .to_string(),
    };
    format!(
        "# gpn-node.toml — written automatically on first run. Safe to edit.\n\
         # A second `gpn_node` start needs no environment variables.\n\
         # The wallet secret is NOT here: it lives in the keystore file below.\n\
         \n\
         base_path   = \"{}\"   # node database + wallet\n\
         chain       = \"{}\"   # chain id or a pinned spec file\n\
         {}\
\
         # Install a service unit if you want the node to start at boot\n\
         # (docs/run-a-node.md, sections Linux / Windows). This flag is\n\
         # informational only — gpn_node never installs a service itself.\n\
         autostart   = false\n",
        toml_escape(&base.to_string_lossy()),
        toml_escape(chain),
        signer_line,
    )
}

/// Write the config only if it does not exist yet. Returns the path when a file
/// was created.
pub fn write_config_if_missing(path: &Path, body: &str) -> Result<bool, String> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, body).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(true)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn td(tag: &str) -> PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("gpn-firstrun-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    const VALID_SPEC: &str = r#"{"name":"GPN Public Testnet","id":"gpn_testnet","genesis":{"runtimeGenesis":{"code":"0x00"}}}"#;

    const ALICE_PHRASE: &str =
        "bottom drive obey lake curtain smoke basket hold race lonely fit walk";

    // ── CLI info: answered before the wizard / network / filesystem ─────────

    fn argv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn info_flags_are_recognised() {
        assert_eq!(
            info_request(&argv(&["gpn_node", "--help"])),
            Some(InfoRequest::Help)
        );
        assert_eq!(
            info_request(&argv(&["gpn_node", "-h"])),
            Some(InfoRequest::Help)
        );
        assert_eq!(
            info_request(&argv(&["gpn_node", "--version"])),
            Some(InfoRequest::Version)
        );
        assert_eq!(
            info_request(&argv(&["gpn_node", "-V"])),
            Some(InfoRequest::Version)
        );
        // The program name itself is never a flag.
        assert_eq!(info_request(&argv(&["gpn_node"])), None);
        assert_eq!(info_request(&argv(&["--setup"])), None);
        assert_eq!(info_request(&argv(&["--helpful"])), None);
    }

    #[test]
    fn version_line_has_the_package_version() {
        let v = version_line();
        assert!(v.starts_with("gpn_node "), "{v}");
        assert!(v.contains(env!("CARGO_PKG_VERSION")), "{v}");
        assert!(v.is_ascii(), "{v}");
        assert_eq!(v.lines().count(), 1, "{v}");
    }

    #[test]
    fn version_line_includes_the_git_sha_when_known() {
        let v = version_line();
        match git_sha() {
            "unknown" => assert!(!v.contains("unknown"), "{v}"),
            sha => assert!(v.contains(sha), "{v}"),
        }
        // Whether or not the SHA is present, the parens stay balanced and there
        // is at most one group.
        assert!(v.matches('(').count() <= 1, "{v}");
        assert_eq!(v.matches('(').count(), v.matches(')').count(), "{v}");
    }

    #[test]
    fn build_id_is_the_marker_the_release_tooling_scans_for() {
        let id = build_id();
        assert!(id.starts_with("gpn-build-v1 version="), "{id}");
        assert!(id.contains(env!("CARGO_PKG_VERSION")), "{id}");
        assert!(id.is_ascii(), "{id}");
        // `git_sha()` must agree with the stamp the artifact carries.
        let sha = git_sha();
        assert!(id.contains(&format!("sha={sha}")), "{id}");
        if sha != "unknown" {
            let core = sha.strip_suffix("-dirty").unwrap_or(sha);
            assert!(
                (7..=40).contains(&core.len()) && core.chars().all(|c| c.is_ascii_hexdigit()),
                "sha looks wrong: {sha:?}"
            );
        }
    }

    #[test]
    fn version_line_formatting_rules() {
        assert_eq!(version_line_with("0.2.0", None, None), "gpn_node 0.2.0");
        assert_eq!(
            version_line_with("0.2.0", Some("41"), None),
            "gpn_node 0.2.0 (spec 41)"
        );
        assert_eq!(
            version_line_with("0.2.0", None, Some("d606264")),
            "gpn_node 0.2.0 (d606264)"
        );
        assert_eq!(
            version_line_with("0.2.0", Some("41"), Some("d606264")),
            "gpn_node 0.2.0 (spec 41, d606264)"
        );
        assert_eq!(
            version_line_with("0.2.0", Some("41"), Some("d606264-dirty")),
            "gpn_node 0.2.0 (spec 41, d606264-dirty)"
        );
        // Builds outside a work tree degrade, never print `unknown`.
        assert_eq!(
            version_line_with("0.2.0", None, Some("unknown")),
            "gpn_node 0.2.0"
        );
        assert_eq!(
            version_line_with("0.2.0", Some("41"), Some("unknown")),
            "gpn_node 0.2.0 (spec 41)"
        );
        assert_eq!(
            version_line_with("0.2.0", Some(""), Some("  ")),
            "gpn_node 0.2.0"
        );
    }

    #[test]
    fn help_text_documents_flags_and_env() {
        let h = help_text();
        assert!(h.is_ascii(), "help must be ASCII");
        assert!(
            h.contains("--version") && h.contains("--help") && h.contains("--setup"),
            "{h}"
        );
        for key in [
            "GPN_BASE_PATH",
            "GPN_CHAIN",
            "GPN_SIGNER",
            "GPN_SIGNER_FILE",
            "GPN_PUBLIC_ADDR",
            "GPN_UPNP",
            "GPN_SEEDS_URL",
            "GPN_BOOTNODES_URL",
            "GPN_REQUIRE_BACKBONE",
            "GPN_SEED",
            "GPN_DISCOVERABLE",
        ] {
            assert!(h.contains(key), "help is missing {key}");
        }
    }

    // ── chain resolution: explicit → file → fetched → builtin ──────────────

    #[test]
    fn explicit_chain_is_untouched_and_never_fetches() {
        let b = td("explicit-name");
        let r = resolve_chain_with(Some("dev".into()), &b, |_| panic!("must not fetch"));
        assert_eq!(r.chain, "dev");
        assert_eq!(r.source, ChainSource::Explicit);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn existing_spec_file_is_reused_without_network() {
        let b = td("specfile");
        std::fs::write(b.join(DEFAULT_SPEC_FILE), VALID_SPEC).unwrap();
        let r = resolve_chain_with(None, &b, |_| panic!("must not fetch"));
        assert_eq!(r.source, ChainSource::SpecFile);
        assert_eq!(
            r.chain,
            b.join(DEFAULT_SPEC_FILE).to_string_lossy().into_owned()
        );
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn missing_spec_is_fetched_and_saved() {
        let b = td("fetch");
        let r = resolve_chain_with(None, &b, |url| {
            assert_eq!(url, GENESIS_SPEC_URL);
            Ok(VALID_SPEC.to_string())
        });
        assert_eq!(r.source, ChainSource::Fetched);
        assert!(b.join(DEFAULT_SPEC_FILE).is_file());
        assert_eq!(r.chain, b.join(DEFAULT_SPEC_FILE).to_string_lossy());
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn fetch_failure_falls_back_to_builtin_testnet() {
        let b = td("fetchfail");
        let r = resolve_chain_with(None, &b, |_| Err("offline".into()));
        assert_eq!(r.source, ChainSource::Builtin);
        assert_eq!(r.chain, "testnet");
        assert!(!b.join(DEFAULT_SPEC_FILE).exists());
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn html_error_page_is_not_accepted_as_a_spec() {
        let b = td("html");
        let r = resolve_chain_with(None, &b, |_| Ok("<html>captive portal</html>".into()));
        assert_eq!(r.source, ChainSource::Builtin);
        assert!(!b.join(DEFAULT_SPEC_FILE).exists());
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn explicit_missing_default_named_spec_refetches() {
        let b = td("explicitmissing");
        let wanted = b.join(DEFAULT_SPEC_FILE);
        let r = resolve_chain_with(Some(wanted.to_string_lossy().into_owned()), &b, |_| {
            Ok(VALID_SPEC.to_string())
        });
        assert_eq!(r.source, ChainSource::Fetched);
        assert!(wanted.is_file());
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn explicit_custom_missing_json_is_not_overwritten() {
        let b = td("custommissing");
        let wanted = b.join("my-spec.json");
        let r = resolve_chain_with(Some(wanted.to_string_lossy().into_owned()), &b, |_| {
            panic!("must not fetch a custom spec")
        });
        assert_eq!(r.source, ChainSource::Explicit);
        assert!(!wanted.exists());
        let _ = std::fs::remove_dir_all(&b);
    }

    // ── wallet generate / import validation ────────────────────────────────

    #[test]
    fn generated_wallet_matches_its_phrase_and_standard_alice() {
        let ks = generate_keystore();
        assert_eq!(ks.crypto, "sr25519");
        assert!(ks.address_is_consistent());
        assert!(ks.address.starts_with('5') || ks.address.starts_with('1'));
        // Well-known dev phrase → well-known ss58 address (subkey fixture).
        assert_eq!(
            ss58_of(ALICE_PHRASE).unwrap(),
            "5DfhGyQdFobKM8NsWvEeAKk5EQQgYe9AydgJ7rMB6E1EqRzV"
        );
    }

    #[test]
    fn a_dev_uri_names_its_account_but_never_creates_one() {
        // The wizard must keep refusing dev shortcuts…
        assert!(ss58_of("//Bob").is_err());
        // …while naming an account that is already configured must work, or a
        // testnet box cannot derive its own address (queue #65: that left the
        // heartbeat without `--address`, so /status advertised gflops 0).
        assert_eq!(
            ss58_of_uri("//Bob").unwrap(),
            "5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty"
        );
        // Real secrets and plain garbage behave as before.
        assert_eq!(
            ss58_of_uri(ALICE_PHRASE).unwrap(),
            ss58_of(ALICE_PHRASE).unwrap()
        );
        assert!(ss58_of_uri("not a key").is_err());
        assert!(ss58_of_uri("").is_err());
    }

    #[test]
    fn hex_seed_import_derives_the_same_pair_as_sp_core() {
        let raw = [7u8; 32];
        let hexseed = format!("0x{}", hex::encode(raw));
        let expected = sr25519::Pair::from_seed_slice(&raw)
            .unwrap()
            .public()
            .to_ss58check();
        assert_eq!(ss58_of(&hexseed).unwrap(), expected);
    }

    #[test]
    fn invalid_secrets_are_rejected_with_actionable_text() {
        // `sp_core::sr25519::Pair` is not Debug, so unwrap_err() can't be used.
        fn err(s: &str) -> String {
            match pair_from_secret(s) {
                Ok(_) => panic!("expected an error for {s:?}"),
                Err(e) => e,
            }
        }
        assert!(err("").contains("empty"));
        assert!(err("//Alice").contains("mnemonic"));
        assert!(err("0x1234").contains("32 bytes"));
        assert!(err("0xzz").contains("hex"));
        let bad_words = "one two three four five six seven eight nine ten eleven";
        assert!(err(bad_words).contains("12/15/18/21/24"));
    }

    #[test]
    fn wizard_generates_and_writes_owner_only_keystore() {
        let b = td("wizgen");
        let path = b.join(KEYSTORE_FILE);
        let mut input = Cursor::new(b"1\n".to_vec());
        let mut out = Vec::new();
        let ks = wallet_wizard(&mut input, &mut out, &path).unwrap();
        let printed = String::from_utf8(out).unwrap();
        assert!(printed.contains("WRITE THIS DOWN"));
        assert!(printed.contains(&ks.mnemonic), "phrase must be shown once");
        assert!(path.is_file());
        assert_eq!(read_keystore(&path).unwrap(), ks);
        // The private fields must not be world readable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "keystore must be 0600, got {mode:04o}");
        }
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn wizard_imports_a_valid_mnemonic() {
        let b = td("wizimp");
        let path = b.join(KEYSTORE_FILE);
        let mut input = Cursor::new(format!("2\n{ALICE_PHRASE}\n").into_bytes());
        let mut out = Vec::new();
        let ks = wallet_wizard(&mut input, &mut out, &path).unwrap();
        assert_eq!(ks.mnemonic, ALICE_PHRASE);
        assert_eq!(ks.address, ss58_of(ALICE_PHRASE).unwrap());
        assert_eq!(read_keystore(&path).unwrap().address, ks.address);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn wizard_retries_a_bad_mnemonic_then_accepts() {
        let b = td("wizretry");
        let path = b.join(KEYSTORE_FILE);
        let mut input = Cursor::new(format!("2\nnot a phrase\n{ALICE_PHRASE}\n").into_bytes());
        let mut out = Vec::new();
        let ks = wallet_wizard(&mut input, &mut out, &path).unwrap();
        assert_eq!(ks.address, ss58_of(ALICE_PHRASE).unwrap());
        assert!(String::from_utf8(out).unwrap().contains("[x]"));
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn wizard_keeps_an_existing_wallet_by_default() {
        let b = td("wizkeep");
        let path = b.join(KEYSTORE_FILE);
        let first = generate_keystore();
        write_keystore(&path, &first).unwrap();
        let mut input = Cursor::new(b"\n".to_vec()); // empty → keep
        let mut out = Vec::new();
        let ks = wallet_wizard(&mut input, &mut out, &path).unwrap();
        assert_eq!(ks, first);
        assert!(String::from_utf8(out).unwrap().contains("Keep it?"));
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn wizard_aborts_on_eof_instead_of_hanging() {
        let b = td("wizeof");
        let path = b.join(KEYSTORE_FILE);
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut out = Vec::new();
        assert!(wallet_wizard(&mut input, &mut out, &path).is_err());
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&b);
    }

    // ── non-TTY refusal ────────────────────────────────────────────────────

    #[test]
    fn non_tty_without_wallet_refuses_with_setup_hint() {
        let e = ensure_interactive(false).unwrap_err();
        assert!(e.contains("not a TTY"));
        assert!(e.contains("gpn_node --setup"));
        // ...and a TTY is allowed through.
        assert!(ensure_interactive(true).is_ok());
    }

    #[test]
    fn tampered_keystore_address_is_rejected() {
        let b = td("tamper");
        let path = b.join(KEYSTORE_FILE);
        let ks = generate_keystore();
        let mut v: serde_json::Value = serde_json::from_str(&ks.to_json()).unwrap();
        v["address"] = serde_json::json!("5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY");
        std::fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();
        assert!(read_keystore(&path).unwrap_err().contains("inconsistent"));
        let _ = std::fs::remove_dir_all(&b);
    }

    // ── config file ────────────────────────────────────────────────────────

    #[test]
    fn config_toml_has_no_secret_and_roundtrips() {
        let b = td("cfg");
        let ks = b.join(KEYSTORE_FILE);
        let body = config_toml(
            Path::new(r"C:\gpn\data"),
            r"C:\gpn\data\testnet-spec.json",
            Some(&ks),
        );
        assert!(!body.contains("//Alice"));
        assert!(!body.contains("mnemonic"));
        assert!(body.contains(r#"base_path   = "C:\\gpn\\data""#));
        assert!(body.contains("signer_file"));
        assert!(body.contains("autostart"));
        // No keystore (dev chain) → the key is only a comment.
        let dev = config_toml(Path::new("/tmp/x"), "dev", None);
        assert!(dev.contains("# signer_file"));
        assert!(!dev
            .lines()
            .any(|l| l.trim_start().starts_with("signer_file")));
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn config_is_written_once_only() {
        let b = td("cfgwrite");
        let p = b.join("node.toml");
        assert!(write_config_if_missing(&p, "a = 1\n").unwrap());
        assert!(!write_config_if_missing(&p, "a = 2\n").unwrap());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "a = 1\n");
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn base_path_defaults_are_platform_specific_and_non_empty() {
        let p = default_base_path();
        assert!(!p.as_os_str().is_empty());
        #[cfg(windows)]
        assert_eq!(p, PathBuf::from(r"C:\gpn\data"));
        #[cfg(not(windows))]
        assert!(p.to_string_lossy().ends_with("/.gpn/node"));
    }

    #[test]
    fn tilde_expansion() {
        #[cfg(unix)]
        {
            std::env::set_var("HOME", "/home/tester");
            assert_eq!(expand_tilde("~/.gpn/node"), "/home/tester/.gpn/node");
        }
        assert_eq!(expand_tilde("/abs/path"), "/abs/path");
    }
}
