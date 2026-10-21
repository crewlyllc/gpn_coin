//! Access-point auth token: resolution, first-boot generation, persistence.
//!
//! The access point (`gpn_scheduler`) requires `Authorization: Bearer <token>`
//! (or `X-Api-Key: <token>`) from **non-loopback** callers. Historically that
//! token existed only if an operator hand-rolled one into
//! `~/.gpn/accesspoint.env` — an unset token silently left a public job API
//! open to the world. So the AP now bootstraps it.
//!
//! **Precedence (highest first):**
//! 1. `GPN_AP_TOKEN` in the process environment — an explicitly supplied token
//!    is never overwritten, and the env file is not touched.
//! 2. `GPN_AP_TOKEN=` in the env file (`$GPN_AP_TOKEN_FILE`, default
//!    `~/.gpn/accesspoint.env`).
//! 3. Generated: 24 CSPRNG bytes as 48 lowercase hex chars (the format the
//!    hand-rolled tokens use), persisted to the env file at mode 0600.
//!
//! Generation happens exactly once. The value is printed once, on the boot
//! that generated it, with the instruction to copy it to the Cloudflare
//! Worker; every later boot prints only a short prefix. Retrieve it any time
//! with `grep GPN_AP_TOKEN ~/.gpn/accesspoint.env`.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Name of the variable, in the environment and in the env file.
pub const VAR: &str = "GPN_AP_TOKEN";
/// Bytes of CSPRNG material in a generated token (→ 48 hex chars).
pub const TOKEN_BYTES: usize = 24;
/// Length of a generated token in characters.
pub const TOKEN_LEN: usize = TOKEN_BYTES * 2;

/// Where the token came from on this boot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// `GPN_AP_TOKEN` was already exported in the process environment.
    Env,
    /// Read from the env file.
    File,
    /// Randomly generated on this boot and persisted to the env file.
    Generated,
}

/// The resolved token plus where it came from.
#[derive(Clone, Debug)]
pub struct Resolved {
    pub token: String,
    pub source: Source,
    /// The env file this token lives in (or was written to).
    pub path: PathBuf,
    /// `Some(reason)` when a generated token could not be persisted — the AP
    /// still runs, but the token will be different on the next boot.
    pub persist_error: Option<String>,
    /// `Some(path)` when an unparseable env file was moved aside before the
    /// generated token was written.
    pub quarantined: Option<PathBuf>,
}

impl Resolved {
    /// A log-safe fingerprint: the first 8 chars for tokens longer than 8,
    /// otherwise just the length (a short token must not be logged whole).
    pub fn masked(&self) -> String {
        let n = self.token.chars().count();
        if n > 8 {
            format!("{}…", self.token.chars().take(8).collect::<String>())
        } else if n == 0 {
            "none".to_string()
        } else {
            format!("set, {n} chars")
        }
    }
}

/// The env file path: `$GPN_AP_TOKEN_FILE`, else `~/.gpn/accesspoint.env`.
pub fn default_path() -> PathBuf {
    if let Some(p) = std::env::var_os("GPN_AP_TOKEN_FILE") {
        let p = PathBuf::from(p);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(format!("{home}/.gpn/accesspoint.env"))
}

/// Resolve the token for this boot, using the real environment and the
/// default env-file path.
pub fn resolve() -> Resolved {
    let env_value = std::env::var(VAR).ok();
    resolve_with(env_value.as_deref(), &default_path())
}

/// Testable core: resolve from an explicit environment value and path.
///
/// Never returns an error: a missing, unreadable, or unparseable env file
/// degrades to "no token on disk" and a fresh token is generated.
pub fn resolve_with(env_value: Option<&str>, path: &Path) -> Resolved {
    // 1. An explicitly supplied token always wins and is never overwritten.
    if let Some(t) = env_value.map(str::trim).filter(|t| !t.is_empty()) {
        return Resolved {
            token: t.to_string(),
            source: Source::Env,
            path: path.to_path_buf(),
            persist_error: None,
            quarantined: None,
        };
    }

    // 2. The env file.
    let (contents, corrupt) = read_env_file(path);
    if let Some(text) = contents.as_deref() {
        if let Some(t) = token_from_env_text(text) {
            return Resolved {
                token: t,
                source: Source::File,
                path: path.to_path_buf(),
                persist_error: None,
                quarantined: None,
            };
        }
    }

    // 3. Generate, then persist.
    let token = match generate_token() {
        Ok(t) => t,
        Err(e) => {
            // Randomness unavailable: run with an empty token (the AP's
            // historical behaviour) rather than refusing to boot.
            eprintln!("  ! could not generate an AP token ({e}); the API stays open to non-loopback callers");
            return Resolved {
                token: String::new(),
                source: Source::Generated,
                path: path.to_path_buf(),
                persist_error: Some(e),
                quarantined: None,
            };
        }
    };

    let mut quarantined = None;
    let mut base = contents.unwrap_or_default();
    if corrupt {
        // Preserve an unparseable file instead of shredding it.
        if let Some(bak) = quarantine(path) {
            quarantined = Some(bak);
            base = String::new();
        }
    }

    let persist_error = match write_token_atomic(path, &base, &token) {
        Ok(()) => None,
        Err(e) => Some(e.to_string()),
    };

    Resolved {
        token,
        source: Source::Generated,
        path: path.to_path_buf(),
        persist_error,
        quarantined,
    }
}

/// Read the env file. Returns `(contents, corrupt)`:
/// * `(None, false)` — file absent (or unreadable for a non-UTF-8/other reason
///   where there is nothing we can safely preserve).
/// * `(Some(text), false)` — valid UTF-8.
/// * `(Some(lossy), true)` — not valid UTF-8 (or held NUL bytes); the caller
///   should move the original aside before rewriting.
fn read_env_file(path: &Path) -> (Option<String>, bool) {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return (None, false),
    };
    match String::from_utf8(bytes.clone()) {
        Ok(t) if !t.contains('\0') => (Some(t), false),
        Ok(t) => (Some(t), true),
        Err(_) => (Some(String::from_utf8_lossy(&bytes).into_owned()), true),
    }
}

/// Find `GPN_AP_TOKEN=<non-empty>` in env-file text. Last occurrence wins,
/// matching `scripts/set-ap-token.sh` (`tail -1`).
pub fn token_from_env_text(text: &str) -> Option<String> {
    let mut found = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue; // e.g. a truncated `GPN_AP_TOKEN` with no '='
        };
        if key.trim() != VAR {
            continue;
        }
        let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
        if !value.is_empty() {
            found = Some(value.to_string());
        }
    }
    found
}

/// 24 CSPRNG bytes → 48 lowercase hex chars.
pub fn generate_token() -> Result<String, String> {
    let mut buf = [0u8; TOKEN_BYTES];
    getrandom::getrandom(&mut buf).map_err(|e| format!("getrandom: {e}"))?;
    Ok(hex::encode(buf))
}

/// Move an unparseable env file aside (`<path>.corrupt.<unix-ts>`).
fn quarantine(path: &Path) -> Option<PathBuf> {
    if !path.exists() {
        return None;
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let bak = PathBuf::from(format!("{}.corrupt.{ts}", path.display()));
    std::fs::rename(path, &bak).ok().map(|_| bak)
}

/// Write `GPN_AP_TOKEN=<token>` into `path`, preserving every other line.
///
/// Atomic: written to a sibling temp file created at mode 0600, fsynced, then
/// renamed over the target. Existing `GPN_AP_TOKEN` lines (set or empty) are
/// replaced by a single line at the end, so `tail -1` stays correct.
fn write_token_atomic(path: &Path, existing: &str, token: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            create_dir_private(dir)?;
        }
    }

    let mut out = String::new();
    for line in existing.lines() {
        let t = line.trim();
        let t = t.strip_prefix("export ").unwrap_or(t);
        if let Some((key, _)) = t.split_once('=') {
            if key.trim() == VAR {
                continue; // replaced below
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&format!("{VAR}={token}\n"));

    let dir = path.parent().filter(|d| !d.as_os_str().is_empty());
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "accesspoint.env".to_string());
    let tmp = match dir {
        Some(d) => d.join(format!(".{file_name}.tmp.{}", std::process::id())),
        None => PathBuf::from(format!(".{file_name}.tmp.{}", std::process::id())),
    };

    let write = || -> std::io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(out.as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)?;
        Ok(())
    };

    match write() {
        Ok(()) => {}
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // The temp file was created 0600; re-assert in case of a pre-existing
        // odd umask/ACL. Failure here is not fatal — the token is on disk.
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[cfg(unix)]
fn create_dir_private(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(not(unix))]
fn create_dir_private(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// The lines to print at startup. Kept separate from printing so the
/// "print the full token exactly once" rule is testable.
pub fn startup_lines(r: &Resolved) -> Vec<String> {
    match r.source {
        Source::Env => vec![format!(
            "  AP token loaded from the environment ({})",
            r.masked()
        )],
        Source::File => vec![format!(
            "  AP token loaded from {} ({})",
            r.path.display(),
            r.masked()
        )],
        Source::Generated if r.token.is_empty() => vec![
            "  AP auth: DISABLED — no token generated, non-loopback callers are not authenticated"
                .to_string(),
        ],
        Source::Generated => {
            let mut v = Vec::new();
            if let Some(bak) = &r.quarantined {
                v.push(format!(
                    "  ! {} was unreadable; kept a copy at {}",
                    r.path.display(),
                    bak.display()
                ));
            }
            v.push("  AP auth: no token in the environment or the env file".to_string());
            v.push(
                "  generated a new AP token — set this as the AP_TOKEN secret on the Cloudflare"
                    .to_string(),
            );
            v.push(
                "  Worker (or run scripts/set-ap-token.sh); it will not be printed again:"
                    .to_string(),
            );
            v.push(String::new());
            v.push(format!("      {VAR}={}", r.token));
            v.push(String::new());
            match &r.persist_error {
                None => v.push(format!("  saved to {} (mode 600)", r.path.display())),
                Some(e) => v.push(format!(
                    "  ! could not save to {} ({e}) — this token is EPHEMERAL and will change on restart",
                    r.path.display()
                )),
            }
            v
        }
    }
}

/// Print [`startup_lines`] to stdout.
pub fn announce(r: &Resolved) {
    for line in startup_lines(r) {
        println!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("gpn-ap-token-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn generated_token_shape() {
        let t = generate_token().unwrap();
        assert_eq!(t.len(), TOKEN_LEN);
        assert!(t
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn parses_token_line_variants() {
        assert_eq!(
            token_from_env_text("GPN_AP_TOKEN=abc\n").as_deref(),
            Some("abc")
        );
        assert_eq!(
            token_from_env_text("export GPN_AP_TOKEN=\"abc\"\n").as_deref(),
            Some("abc")
        );
        assert_eq!(token_from_env_text("GPN_AP_TOKEN=\n"), None);
        assert_eq!(token_from_env_text("GPN_AP_TOKEN=   \n"), None);
        assert_eq!(token_from_env_text("GPN_AP_TOKEN\n"), None);
        assert_eq!(token_from_env_text("# GPN_AP_TOKEN=abc\n"), None);
        assert_eq!(
            token_from_env_text("GPN_AP_TOKEN=a\nGPN_AP_TOKEN=b\n").as_deref(),
            Some("b")
        );
    }
}
