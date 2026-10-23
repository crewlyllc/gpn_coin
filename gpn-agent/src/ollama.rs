//! Ollama — a **bundled, REQUIRED** part of a GPN node.
//!
//! Policy (Jeremy): a GPN node is *always* miner **and** AI provider. Serving
//! LLM jobs requires an ollama server; a node that cannot reach one is only
//! half a node. Therefore ollama is a **first-class prerequisite**:
//!
//! * the installers ship inside the release package (`scripts/install-ollama.*`),
//! * `gpn_node --setup` (and every normal start) probes
//!   `GPN_OLLAMA` (default <http://127.0.0.1:11434>), and if it is missing
//!   attempts an automated bootstrap: install → `ollama serve` (detached) →
//!   wait for `/api/tags` → pull the baseline model,
//! * and — the **hard gate** — if ollama is still not reachable afterwards the
//!   supervisor **refuses to start** with a loud, actionable error and a
//!   non-zero exit ([`EXIT_NO_OLLAMA`]). It never silently runs miner-only.
//!
//! `GPN_REQUIRE_OLLAMA=0` (`false`/`no`/`off`) is the documented opt-out for
//! special cases (CI smoke tests, a box that only mines on purpose); it
//! downgrades a missing ollama from a hard failure to a prominent warning.
//!
//! Related envs:
//!
//! | Env | Meaning | Default |
//! |-----|---------|---------|
//! | `GPN_OLLAMA` | ollama base URL | `http://127.0.0.1:11434` |
//! | `GPN_REQUIRE_OLLAMA` | hard gate on/off | `1` (ON) |
//! | `GPN_OLLAMA_SETUP` | attempt install/serve at startup | `1` (ON) |
//! | `GPN_BASELINE_MODELS` | model(s) to pull, comma-separated | `llama3.2:1b` |
//! | `GPN_OLLAMA_WAIT_SECS` | wait for `/api/tags` after starting | `60` |
//! | `GPN_OLLAMA_PULL_TIMEOUT_SECS` | max time for one model pull | `900` |
//!
//! The pure decision functions ([`classify`], [`gate`], [`require_ollama`])
//! are unit-tested without a network; [`ensure_with`] is stub-tested through
//! the [`Hooks`] trait so the setup path is exercised without side effects.

use std::time::Duration;

/// Default ollama base URL.
pub const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:11434";

/// Default baseline model pulled on first run (small; good CPU fallback).
pub const DEFAULT_BASELINE: &str = "llama3.2:1b";

/// Distinct exit code when a node refuses to start because ollama is missing.
///
/// 69 is `EX_UNAVAILABLE` (sysexits.h): "a service is unavailable". The
/// supervisor prints `node=down(no ollama)` before exiting with this code so
/// an outer service manager surfaces the cause instead of a generic crash.
pub const EXIT_NO_OLLAMA: i32 = 69;

/// Default grace window (seconds) for ollama to answer `/api/tags`.
pub const DEFAULT_WAIT_SECS: u64 = 60;

/// Default timeout (seconds) for one `ollama pull` / `/api/pull`.
pub const DEFAULT_PULL_TIMEOUT_SECS: u64 = 900;

/// How long a single reachability probe may take.
const PROBE_TIMEOUT_MS: u64 = 1500;

// ─────────────────────────────────────────────────────────────────────────────
// Classification (pure)
// ─────────────────────────────────────────────────────────────────────────────

/// Result of probing the ollama HTTP API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// `/api/tags` answered 2xx with the expected JSON shape.
    Serving { models: Vec<String> },
    /// Something answers, but it is not a healthy ollama API.
    Unhealthy { detail: String },
    /// Nothing answers (connection refused / timeout / DNS).
    Unreachable { detail: String },
}

impl Probe {
    pub fn serving(&self) -> bool {
        matches!(self, Probe::Serving { .. })
    }
    pub fn models(&self) -> &[String] {
        match self {
            Probe::Serving { models } => models,
            _ => &[],
        }
    }
    pub fn detail(&self) -> &str {
        match self {
            Probe::Serving { .. } => "serving",
            Probe::Unhealthy { detail } | Probe::Unreachable { detail } => detail,
        }
    }
}

/// Classification of an HTTP response (or lack of one) from `/api/tags`.
///
/// Pure so it can be unit-tested without a server:
/// * no status → [`Probe::Unreachable`] (connection error),
/// * non-2xx → [`Probe::Unhealthy`],
/// * 2xx with `{"models":[…]}` → [`Probe::Serving`],
/// * 2xx with anything else → [`Probe::Unhealthy`] (some other server on :11434).
pub fn classify(status: Option<u16>, body: Option<&str>) -> Probe {
    let Some(code) = status else {
        return Probe::Unreachable {
            detail: "no response".to_string(),
        };
    };
    if !(200..300).contains(&code) {
        return Probe::Unhealthy {
            detail: format!("HTTP {code} from /api/tags"),
        };
    }
    let Some(body) = body else {
        return Probe::Unhealthy {
            detail: "empty body from /api/tags".to_string(),
        };
    };
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(v) => match v.get("models") {
            Some(serde_json::Value::Array(arr)) => Probe::Serving {
                models: arr
                    .iter()
                    .filter_map(|m| m.get("name").and_then(|n| n.as_str()))
                    .map(str::to_string)
                    .collect(),
            },
            _ => Probe::Unhealthy {
                detail: "JSON without a `models` array".to_string(),
            },
        },
        Err(_) => Probe::Unhealthy {
            detail: "non-JSON body from /api/tags".to_string(),
        },
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Policy (pure)
// ─────────────────────────────────────────────────────────────────────────────

/// Parse `GPN_REQUIRE_OLLAMA`. Default **ON**; only an explicit false-y value
/// opts out (mirrors `GPN_REQUIRE_DIALABLE`).
pub fn require_ollama(raw: Option<&str>) -> bool {
    !matches!(
        raw.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        Some("0" | "false" | "no" | "off")
    )
}

/// The hard-gate decision. Pure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    /// ollama is serving — start normally.
    Allow,
    /// ollama is missing but the gate is opted out — start, loudly degraded.
    AllowDegraded,
    /// ollama is missing and required — refuse to start.
    Refuse,
}

/// Decide whether the node may start, given the requirement flag and whether
/// ollama ended up serving after the bootstrap attempt.
pub fn gate(require: bool, serving: bool) -> GateDecision {
    match (require, serving) {
        (_, true) => GateDecision::Allow,
        (true, false) => GateDecision::Refuse,
        (false, false) => GateDecision::AllowDegraded,
    }
}

/// Split a comma/semicolon separated model list.
pub fn parse_models(raw: Option<&str>) -> Vec<String> {
    let Some(raw) = raw else {
        return vec![DEFAULT_BASELINE.to_string()];
    };
    let models: Vec<String> = raw
        .split([',', ';'])
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .collect();
    if models.is_empty() {
        vec![DEFAULT_BASELINE.to_string()]
    } else {
        models
    }
}

/// Is `model` present in `models`? Ollama reports names with an implicit
/// `:latest` tag, so `llama3.2` and `llama3.2:latest` are equivalent.
pub fn model_present(models: &[String], model: &str) -> bool {
    let norm = |m: &str| {
        let m = m.trim();
        if m.contains(':') {
            m.to_string()
        } else {
            format!("{m}:latest")
        }
    };
    let want = norm(model);
    models.iter().any(|m| norm(m) == want)
}

// ─────────────────────────────────────────────────────────────────────────────
// Setup options + outcome
// ─────────────────────────────────────────────────────────────────────────────

/// Knobs for one setup attempt.
#[derive(Debug, Clone)]
pub struct SetupOpts {
    pub endpoint: String,
    /// Models that should exist for the node to be useful on first run.
    pub baseline: Vec<String>,
    /// Allow the installer/bootstrap to run (env `GPN_OLLAMA_SETUP`).
    pub allow_install: bool,
    /// Pull missing baseline models (on `--setup` / after a fresh install).
    pub pull_missing: bool,
    /// Grace window for `/api/tags` to come up after starting the server.
    pub wait_secs: u64,
    /// Timeout for a single model pull.
    pub pull_timeout_secs: u64,
}

impl Default for SetupOpts {
    fn default() -> Self {
        SetupOpts {
            endpoint: DEFAULT_ENDPOINT.to_string(),
            baseline: vec![DEFAULT_BASELINE.to_string()],
            allow_install: true,
            pull_missing: false,
            wait_secs: DEFAULT_WAIT_SECS,
            pull_timeout_secs: DEFAULT_PULL_TIMEOUT_SECS,
        }
    }
}

impl SetupOpts {
    /// Resolve from the environment, taking the endpoint from the node config.
    pub fn from_env(endpoint: &str, pull_missing: bool) -> Self {
        let d = SetupOpts::default();
        SetupOpts {
            endpoint: if endpoint.trim().is_empty() {
                d.endpoint
            } else {
                endpoint.trim().trim_end_matches('/').to_string()
            },
            baseline: parse_models(std::env::var("GPN_BASELINE_MODELS").ok().as_deref()),
            allow_install: match std::env::var("GPN_OLLAMA_SETUP") {
                Ok(v) => !matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "0" | "false" | "no" | "off"
                ),
                Err(_) => true,
            },
            pull_missing,
            wait_secs: std::env::var("GPN_OLLAMA_WAIT_SECS")
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
                .map(|s| s.max(5))
                .unwrap_or(DEFAULT_WAIT_SECS),
            pull_timeout_secs: std::env::var("GPN_OLLAMA_PULL_TIMEOUT_SECS")
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
                .map(|s| s.max(30))
                .unwrap_or(DEFAULT_PULL_TIMEOUT_SECS),
        }
    }
}

/// What happened during one setup attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupOutcome {
    /// ollama answered `/api/tags` at the end of the attempt.
    pub serving: bool,
    /// We invoked an installer (or would have, if it was required).
    pub attempted_install: bool,
    /// The installer actually installed ollama (it was absent before).
    pub installed: bool,
    /// Every baseline model is present (meaningful only when `serving`).
    pub model_ready: bool,
    /// Models reported by `/api/tags` at the end.
    pub models: Vec<String>,
    /// Human-readable summary for logs.
    pub detail: String,
}

impl SetupOutcome {
    fn refused(detail: impl Into<String>, attempted_install: bool) -> Self {
        SetupOutcome {
            serving: false,
            attempted_install,
            installed: false,
            model_ready: false,
            models: Vec::new(),
            detail: detail.into(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Side-effect hooks (real impl below, stubs in tests)
// ─────────────────────────────────────────────────────────────────────────────

/// All side effects of the bootstrap go through this trait so the setup path
/// is unit-testable without touching the host.
pub trait Hooks {
    /// One `/api/tags` probe.
    fn probe(&mut self, endpoint: &str) -> Probe;
    /// Install ollama if absent. `Ok(true)` = newly installed, `Ok(false)` =
    /// already present. `Err` carries the reason.
    fn install(&mut self) -> Result<bool, String>;
    /// Start `ollama serve` detached (no-op if already serving).
    fn start(&mut self) -> Result<(), String>;
    /// Pull one model through the ollama API.
    fn pull(&mut self, endpoint: &str, model: &str, timeout_secs: u64) -> Result<(), String>;
    /// Sleep between probes (seconds).
    fn sleep(&mut self, secs: u64);
    /// Progress line (already prefixed by the caller-facing impl).
    fn log(&mut self, msg: &str);
}

/// Run the setup state machine against arbitrary [`Hooks`].
///
/// Order of operations:
/// 1. probe — if serving, do not touch anything (just report inventory);
/// 2. if not serving and installs are allowed: install → start → poll
///    `/api/tags` up to `wait_secs`;
/// 3. if serving and `pull_missing`: pull each missing baseline model;
/// 4. report the final outcome.
pub fn ensure_with<H: Hooks>(opts: &SetupOpts, hooks: &mut H) -> SetupOutcome {
    let endpoint = opts.endpoint.trim_end_matches('/').to_string();
    let mut probe = hooks.probe(&endpoint);
    let mut attempted_install = false;
    let mut installed = false;

    if !probe.serving() && opts.allow_install {
        attempted_install = true;
        match hooks.install() {
            Ok(new) => {
                installed = new;
                if new {
                    hooks.log("installed ollama");
                } else {
                    hooks.log("ollama binary already present");
                }
            }
            Err(e) => {
                hooks.log(&format!("install failed: {e}"));
            }
        }
        if let Err(e) = hooks.start() {
            hooks.log(&format!("could not start `ollama serve`: {e}"));
        }
        let deadline = opts.wait_secs.max(5);
        let mut waited = 0;
        while !probe.serving() && waited < deadline {
            let step = 2.min(deadline - waited).max(1);
            hooks.sleep(step);
            waited += step;
            probe = hooks.probe(&endpoint);
        }
    } else if !probe.serving() {
        hooks.log("GPN_OLLAMA_SETUP=0 — skipping the automated bootstrap");
    }

    if !probe.serving() {
        return SetupOutcome::refused(
            format!("ollama is not reachable at {endpoint} ({})", probe.detail()),
            attempted_install,
        );
    }

    let mut models: Vec<String> = probe.models().to_vec();
    let mut missing: Vec<String> = opts
        .baseline
        .iter()
        .filter(|m| !model_present(&models, m))
        .cloned()
        .collect();

    if opts.pull_missing {
        for m in missing.clone() {
            hooks.log(&format!("pulling baseline model: {m}"));
            match hooks.pull(&endpoint, &m, opts.pull_timeout_secs) {
                Ok(()) => {
                    if !model_present(&models, &m) {
                        models.push(m.clone());
                    }
                }
                Err(e) => hooks.log(&format!("pull failed ({m}): {e}")),
            }
        }
        missing = opts
            .baseline
            .iter()
            .filter(|m| !model_present(&models, m))
            .cloned()
            .collect();
    }

    let model_ready = missing.is_empty();
    let detail = if model_ready {
        format!("ollama serving at {endpoint}")
    } else {
        format!(
            "ollama serving at {endpoint}; missing baseline model(s): {}",
            missing.join(", ")
        )
    };

    SetupOutcome {
        serving: true,
        attempted_install,
        installed,
        model_ready,
        models,
        detail,
    }
}

/// Run the setup with the real system hooks.
pub fn ensure(opts: &SetupOpts) -> SetupOutcome {
    let mut hooks = SystemHooks::new();
    ensure_with(opts, &mut hooks)
}

// ─────────────────────────────────────────────────────────────────────────────
// Real system hooks
// ─────────────────────────────────────────────────────────────────────────────

/// Real-world [`Hooks`]: HTTP probe, `winget`/installer, detached serve, pull.
pub struct SystemHooks {
    /// Path to the ollama executable once found (Windows fallback path).
    ollama_bin: Option<String>,
}

impl Default for SystemHooks {
    fn default() -> Self {
        SystemHooks::new()
    }
}

impl SystemHooks {
    pub fn new() -> Self {
        SystemHooks { ollama_bin: None }
    }

    /// Locate the ollama executable (PATH, plus the Windows per-user install
    /// dir which is not on the runner's PATH right after a fresh install).
    fn find_ollama(&mut self) -> Option<String> {
        if let Some(b) = &self.ollama_bin {
            if std::path::Path::new(b).is_file() {
                return Some(b.clone());
            }
        }
        if let Some(p) = which("ollama") {
            self.ollama_bin = Some(p.clone());
            return Some(p);
        }
        #[cfg(windows)]
        {
            if let Ok(local) = std::env::var("LOCALAPPDATA") {
                let p = std::path::PathBuf::from(local)
                    .join("Programs")
                    .join("Ollama")
                    .join("ollama.exe");
                if p.is_file() {
                    let s = p.to_string_lossy().into_owned();
                    self.ollama_bin = Some(s.clone());
                    return Some(s);
                }
            }
        }
        #[cfg(not(windows))]
        {
            // `install.sh` installs ollama user-local into `$GPN_PREFIX/ollama`
            // (not necessarily on PATH); also pick up the usual bin dirs.
            let prefix = std::env::var("GPN_PREFIX").unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_default();
                format!("{home}/.gpn")
            });
            let home = std::env::var("HOME").unwrap_or_default();
            let candidates = [
                format!("{prefix}/ollama/bin/ollama"),
                format!("{home}/.local/bin/ollama"),
                "/usr/local/bin/ollama".to_string(),
                "/opt/homebrew/bin/ollama".to_string(),
                "/usr/bin/ollama".to_string(),
            ];
            for c in candidates {
                if std::path::Path::new(&c).is_file() {
                    self.ollama_bin = Some(c.clone());
                    return Some(c);
                }
            }
        }
        None
    }
}

impl Hooks for SystemHooks {
    fn probe(&mut self, endpoint: &str) -> Probe {
        let base = endpoint.trim_end_matches('/');
        let client = match reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(PROBE_TIMEOUT_MS))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                return Probe::Unreachable {
                    detail: format!("http client: {e}"),
                }
            }
        };
        match client.get(format!("{base}/api/tags")).send() {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let body = resp.text().unwrap_or_default();
                classify(Some(status), Some(&body))
            }
            Err(e) => Probe::Unreachable {
                detail: format!("{e}"),
            },
        }
    }

    fn install(&mut self) -> Result<bool, String> {
        if self.find_ollama().is_some() {
            return Ok(false);
        }
        #[cfg(windows)]
        {
            // 1) winget (preferred on Windows 10/11).
            let winget_ok = std::process::Command::new("winget")
                .args([
                    "install",
                    "--id",
                    "Ollama.Ollama",
                    "-e",
                    "--accept-source-agreements",
                    "--accept-package-agreements",
                    "--disable-interactivity",
                ])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if winget_ok && self.find_ollama().is_some() {
                return Ok(true);
            }
            // 2) official OllamaSetup.exe (silent).
            let setup = std::env::temp_dir().join("OllamaSetup.exe");
            let ps = format!(
                "$ErrorActionPreference='Stop'; Invoke-WebRequest -Uri 'https://ollama.com/download/OllamaSetup.exe' -OutFile '{}'; Start-Process -FilePath '{}' -ArgumentList '/VERYSILENT' -Wait",
                setup.display(),
                setup.display()
            );
            let ok = std::process::Command::new("powershell")
                .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &ps])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if ok && self.find_ollama().is_some() {
                return Ok(true);
            }
            Err("winget and the official installer both failed".to_string())
        }
        #[cfg(not(windows))]
        {
            // Prefer the bundled installer (shipped in the release package).
            if let Some(script) = find_script("install-ollama.sh") {
                let ok = std::process::Command::new("sh")
                    .arg(&script)
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
                if ok && self.find_ollama().is_some() {
                    return Ok(true);
                }
                return Err(format!("bundled installer {script} failed"));
            }
            // Fallback: the official one-liner installer.
            let ok = std::process::Command::new("sh")
                .args(["-c", "curl -fsSL https://ollama.com/install.sh | sh"])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if ok && self.find_ollama().is_some() {
                Ok(true)
            } else {
                Err("no bundled installer and the official install script failed".to_string())
            }
        }
    }

    fn start(&mut self) -> Result<(), String> {
        if self.probe(DEFAULT_ENDPOINT).serving() {
            return Ok(());
        }
        let bin = self
            .find_ollama()
            .ok_or_else(|| "ollama executable not found".to_string())?;

        #[cfg(windows)]
        {
            // Detached: `cmd /c start "" /B ollama serve` survives this process.
            let mut cmd = std::process::Command::new("cmd");
            cmd.args(["/C", "start", "", "/B", &bin, "serve"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            use std::os::windows::process::CommandExt;
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);
            cmd.spawn()
                .map_err(|e| format!("spawn `{bin} serve`: {e}"))?;
            Ok(())
        }
        #[cfg(not(windows))]
        {
            let log = format!(
                "{}/.gpn/ollama.log",
                std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string())
            );
            let _ = std::fs::create_dir_all(
                std::path::Path::new(&log)
                    .parent()
                    .unwrap_or(std::path::Path::new("/tmp")),
            );
            let sh = format!(
                "nohup {bin} serve >> {log} 2>&1 & echo $!",
                bin = shell_quote(&bin),
                log = shell_quote(&log)
            );
            let out = std::process::Command::new("sh")
                .args(["-c", &sh])
                .output()
                .map_err(|e| format!("spawn `ollama serve`: {e}"))?;
            if !out.status.success() {
                return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
            }
            Ok(())
        }
    }

    fn pull(&mut self, endpoint: &str, model: &str, timeout_secs: u64) -> Result<(), String> {
        let base = endpoint.trim_end_matches('/');
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client
            .post(format!("{base}/api/pull"))
            .json(&serde_json::json!({ "model": model, "stream": false }))
            .send()
            .map_err(|e| e.to_string())?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(format!("HTTP {}", resp.status()))
        }
    }

    fn sleep(&mut self, secs: u64) {
        std::thread::sleep(Duration::from_secs(secs));
    }

    fn log(&mut self, msg: &str) {
        eprintln!("[ollama] {msg}");
    }
}

/// `which`-style lookup of an executable on `$PATH`.
fn which(name: &str) -> Option<String> {
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        for cand in [dir.join(name), dir.join(format!("{name}.exe"))] {
            if cand.is_file() {
                return Some(cand.to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// Locate a bundled script near the binary / cwd (`scripts/<name>`), also
/// honouring `GPN_INSTALLER_DIR` for unusual deployments.
pub fn find_script(name: &str) -> Option<String> {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(d) = std::env::var("GPN_INSTALLER_DIR") {
        dirs.push(std::path::PathBuf::from(d));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(p) = exe.parent() {
            dirs.push(p.to_path_buf());
            dirs.push(p.join("scripts"));
            if let Some(up) = p.parent() {
                dirs.push(up.join("scripts"));
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd.join("scripts"));
        dirs.push(cwd.clone());
    }
    for d in dirs {
        let p = d.join(name);
        if p.is_file() {
            return Some(p.to_string_lossy().into_owned());
        }
    }
    None
}

/// POSIX shell single-quote.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── policy ──────────────────────────────────────────────────────────────

    #[test]
    fn require_defaults_on() {
        assert!(require_ollama(None));
        assert!(require_ollama(Some("")));
        assert!(require_ollama(Some("1")));
        assert!(require_ollama(Some("true")));
        assert!(require_ollama(Some("yes")));
        assert!(require_ollama(Some("bogus")));
    }

    #[test]
    fn require_opt_out_values() {
        for v in ["0", "false", "FALSE", " no ", "off", "Off"] {
            assert!(!require_ollama(Some(v)), "{v} should opt out");
        }
    }

    #[test]
    fn gate_refuses_only_when_required_and_missing() {
        assert_eq!(gate(true, true), GateDecision::Allow);
        assert_eq!(gate(false, true), GateDecision::Allow);
        assert_eq!(gate(true, false), GateDecision::Refuse);
        assert_eq!(gate(false, false), GateDecision::AllowDegraded);
    }

    // ── classification ──────────────────────────────────────────────────────

    #[test]
    fn classify_no_response_is_unreachable() {
        assert!(matches!(classify(None, None), Probe::Unreachable { .. }));
    }

    #[test]
    fn classify_non_2xx_is_unhealthy() {
        assert!(matches!(
            classify(Some(500), Some("{}")),
            Probe::Unhealthy { .. }
        ));
        assert!(matches!(classify(Some(404), None), Probe::Unhealthy { .. }));
    }

    #[test]
    fn classify_2xx_with_models_array_is_serving() {
        let p = classify(
            Some(200),
            Some(r#"{"models":[{"name":"llama3.2:1b"},{"name":"qwen2.5:0.5b"}]}"#),
        );
        assert_eq!(
            p,
            Probe::Serving {
                models: vec!["llama3.2:1b".into(), "qwen2.5:0.5b".into()]
            }
        );
        assert!(p.serving());
        assert_eq!(p.models().len(), 2);
    }

    #[test]
    fn classify_2xx_without_models_is_unhealthy() {
        // Some other service (or a proxy error page) on the port.
        assert!(matches!(
            classify(Some(200), Some("<html>hi</html>")),
            Probe::Unhealthy { .. }
        ));
        assert!(matches!(
            classify(Some(200), Some(r#"{"models":{}}"#)),
            Probe::Unhealthy { .. }
        ));
        assert!(matches!(
            classify(Some(200), Some(r#"{"error":"x"}"#)),
            Probe::Unhealthy { .. }
        ));
        assert!(matches!(classify(Some(200), None), Probe::Unhealthy { .. }));
    }

    // ── models ──────────────────────────────────────────────────────────────

    #[test]
    fn parse_models_defaults_and_splits() {
        assert_eq!(parse_models(None), vec![DEFAULT_BASELINE.to_string()]);
        assert_eq!(parse_models(Some("  ")), vec![DEFAULT_BASELINE.to_string()]);
        assert_eq!(
            parse_models(Some("llama3.2:1b, qwen2.5:0.5b")),
            vec!["llama3.2:1b", "qwen2.5:0.5b"]
        );
    }

    #[test]
    fn model_present_normalises_latest_tag() {
        let have = vec!["llama3.2:latest".to_string()];
        assert!(model_present(&have, "llama3.2"));
        assert!(model_present(&have, "llama3.2:latest"));
        assert!(!model_present(&have, "llama3.2:1b"));
        assert!(!model_present(&[], "llama3.2:1b"));
    }

    // ── setup state machine (stubbed) ───────────────────────────────────────

    /// Stub hooks: a scripted sequence of probe results + install/start/pull
    /// behaviour. Never touches the network or the filesystem.
    struct Stub {
        /// Probe results consumed in order; the last one repeats.
        probes: Vec<Probe>,
        cursor: usize,
        install_result: Option<Result<bool, String>>,
        start_result: Result<(), String>,
        pulls: Vec<String>,
        pull_fails: bool,
        pub logs: Vec<String>,
        sleeps: u64,
    }

    impl Stub {
        fn new(probes: Vec<Probe>) -> Self {
            Stub {
                probes,
                cursor: 0,
                install_result: None,
                start_result: Ok(()),
                pulls: Vec::new(),
                pull_fails: false,
                logs: Vec::new(),
                sleeps: 0,
            }
        }
        fn next_probe(&mut self) -> Probe {
            let p = self
                .probes
                .get(self.cursor)
                .cloned()
                .or_else(|| self.probes.last().cloned())
                .unwrap_or(Probe::Unreachable {
                    detail: "stub".into(),
                });
            if self.cursor + 1 < self.probes.len() {
                self.cursor += 1;
            }
            p
        }
    }

    impl Hooks for Stub {
        fn probe(&mut self, _endpoint: &str) -> Probe {
            self.next_probe()
        }
        fn install(&mut self) -> Result<bool, String> {
            self.install_result.clone().unwrap_or(Ok(true))
        }
        fn start(&mut self) -> Result<(), String> {
            self.start_result.clone()
        }
        fn pull(&mut self, _endpoint: &str, model: &str, _t: u64) -> Result<(), String> {
            if self.pull_fails {
                Err("stub pull failure".into())
            } else {
                self.pulls.push(model.to_string());
                Ok(())
            }
        }
        fn sleep(&mut self, secs: u64) {
            self.sleeps += secs;
        }
        fn log(&mut self, msg: &str) {
            self.logs.push(msg.to_string());
        }
    }

    fn serving(models: &[&str]) -> Probe {
        Probe::Serving {
            models: models.iter().map(|s| s.to_string()).collect(),
        }
    }
    fn down() -> Probe {
        Probe::Unreachable {
            detail: "connection refused".into(),
        }
    }

    #[test]
    fn already_serving_is_a_no_op() {
        let mut stub = Stub::new(vec![serving(&["llama3.2:1b"])]);
        let opts = SetupOpts::default();
        let out = ensure_with(&opts, &mut stub);
        assert!(out.serving);
        assert!(!out.attempted_install, "must not install when already up");
        assert!(out.model_ready);
        assert!(stub.pulls.is_empty());
        assert_eq!(stub.sleeps, 0);
    }

    #[test]
    fn missing_ollama_bootstraps_then_serves() {
        // down → down → serving (install + start fix it during the wait loop)
        let mut stub = Stub::new(vec![down(), down(), serving(&["llama3.2:1b"])]);
        stub.install_result = Some(Ok(true));
        let opts = SetupOpts {
            wait_secs: 10,
            ..SetupOpts::default()
        };
        let out = ensure_with(&opts, &mut stub);
        assert!(out.serving);
        assert!(out.attempted_install);
        assert!(out.installed);
        assert!(out.model_ready);
        assert!(stub.sleeps > 0);
    }

    #[test]
    fn install_failure_still_refuses() {
        let mut stub = Stub::new(vec![down()]);
        stub.install_result = Some(Err("no network".into()));
        stub.start_result = Err("not found".into());
        let opts = SetupOpts {
            wait_secs: 6,
            ..SetupOpts::default()
        };
        let out = ensure_with(&opts, &mut stub);
        assert!(!out.serving);
        assert!(out.attempted_install);
        assert!(!out.installed);
        assert_eq!(gate(true, out.serving), GateDecision::Refuse);
        assert_eq!(gate(false, out.serving), GateDecision::AllowDegraded);
        // It really did wait/poll rather than giving up instantly.
        assert!(stub.sleeps >= 6);
    }

    #[test]
    fn setup_disabled_does_not_install() {
        let mut stub = Stub::new(vec![down()]);
        let opts = SetupOpts {
            allow_install: false,
            ..SetupOpts::default()
        };
        let out = ensure_with(&opts, &mut stub);
        assert!(!out.serving);
        assert!(!out.attempted_install);
        assert_eq!(stub.sleeps, 0);
    }

    #[test]
    fn pull_missing_only_when_asked() {
        // Not asked: report the gap, do not pull.
        let mut stub = Stub::new(vec![serving(&[])]);
        let out = ensure_with(&SetupOpts::default(), &mut stub);
        assert!(out.serving);
        assert!(!out.model_ready);
        assert!(stub.pulls.is_empty());

        // Asked: pull exactly the missing baseline models.
        let mut stub = Stub::new(vec![serving(&["qwen2.5:0.5b"])]);
        let opts = SetupOpts {
            baseline: vec!["llama3.2:1b".into(), "qwen2.5:0.5b".into()],
            pull_missing: true,
            ..SetupOpts::default()
        };
        let out = ensure_with(&opts, &mut stub);
        assert!(out.serving);
        assert!(out.model_ready);
        assert_eq!(stub.pulls, vec!["llama3.2:1b"]);
    }

    #[test]
    fn pull_failure_leaves_model_not_ready_but_serving() {
        let mut stub = Stub::new(vec![serving(&[])]);
        stub.pull_fails = true;
        let opts = SetupOpts {
            pull_missing: true,
            ..SetupOpts::default()
        };
        let out = ensure_with(&opts, &mut stub);
        assert!(out.serving, "a failed pull must not fail the gate");
        assert!(!out.model_ready);
    }

    #[test]
    fn fresh_install_pulls_baseline() {
        // Installed from scratch, then asked to pull the baseline.
        let mut stub = Stub::new(vec![down(), serving(&[])]);
        stub.install_result = Some(Ok(true));
        let opts = SetupOpts {
            wait_secs: 10,
            pull_missing: true,
            ..SetupOpts::default()
        };
        let out = ensure_with(&opts, &mut stub);
        assert!(out.serving && out.installed && out.model_ready);
        assert_eq!(stub.pulls, vec![DEFAULT_BASELINE.to_string()]);
    }

    #[test]
    fn unhealthy_is_not_serving() {
        let mut stub = Stub::new(vec![Probe::Unhealthy {
            detail: "HTTP 500".into(),
        }]);
        let opts = SetupOpts {
            wait_secs: 5,
            ..SetupOpts::default()
        };
        let out = ensure_with(&opts, &mut stub);
        assert!(!out.serving);
        assert!(out.detail.contains("not reachable"));
    }

    #[test]
    fn script_lookup_respects_installer_dir() {
        let dir = std::env::temp_dir().join(format!("gpn-ollama-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("install-ollama.sh");
        std::fs::write(&script, "#!/bin/sh\n").unwrap();
        // SAFETY: tests in this module do not run concurrently on this var.
        std::env::set_var("GPN_INSTALLER_DIR", &dir);
        let found = find_script("install-ollama.sh");
        std::env::remove_var("GPN_INSTALLER_DIR");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(found.is_some());
        assert!(found.unwrap().ends_with("install-ollama.sh"));
    }
}
