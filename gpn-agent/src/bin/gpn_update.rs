//! `gpn_update` — the deployment-mode-independent updater.
//!
//! ## Why this binary exists
//!
//! Auto-update used to live only inside `gpn_node` (the installer's
//! supervisor). A host that runs `gpn_fleet` instead — the fleet directive,
//! `GPN_MINER_ACCOUNT`, challenge-verifier secrets — never starts `gpn_node`,
//! so **nothing ever checked the signed manifest** and its binaries froze at
//! whatever the tarball contained. Replacing the fleet with `gpn_node` is not
//! an option (it would lose the fleet directive), so the check has to be a
//! process of its own.
//!
//! This binary does exactly what `gpn_node`'s auto-updater does, using the same
//! `gpn_agent::update` code — manifest parsing, ed25519 verification against
//! the compiled-in release key, size+sha256 checks, staged extraction into
//! `versions/<v>/`, the `current` flip, `current.previous`, and pruning. It adds
//! the one step `gpn_node` does not need: copying the new binaries over the
//! **install directory** the operator actually runs from (`~/.local/bin` by
//! default), atomically per file.
//!
//! ```text
//!   gpn_update                 # = --check
//!   gpn_update --check         # fetch + verify the manifest, never download
//!   gpn_update --apply         # download, verify, install, copy, (optionally) restart
//!   gpn_update --check --json  # machine-readable single JSON object
//!   gpn_update --apply --quiet # the systemd timer's mode (no stdout)
//! ```
//!
//! ## Exit codes (stable; scripts and the timer depend on them)
//!
//! `--check`:
//! * `0` — up to date (no newer verified release)
//! * `1` — error (bad signature/key/JSON, no asset for this platform, I/O)
//! * `2` — the manifest could not be fetched (offline, DNS, HTTP != 2xx)
//! * `3` — a **newer verified** release exists (this is the "update me" signal)
//!
//! `--apply`:
//! * `0` — applied, or already up to date
//! * `1` — error (nothing was swapped, or the copy/restart step failed)
//! * `2` — the manifest could not be fetched
//! * `3` — a newer release exists but was **not** applied (`GPN_AUTO_UPDATE=0`,
//!   or `--check`-style policy refusal)
//!
//! ## Environment
//!
//! | Variable | Meaning |
//! |---|---|
//! | `GPN_UPDATE_URL` | manifest URL (default [`update::MANIFEST_URL`]) |
//! | `GPN_UPDATE_DIR` | state dir: `versions/`, `current`, `current.previous` (default `$GPN_PREFIX/update`, else `~/.gpn/update`) |
//! | `GPN_UPDATE_TARGET_DIR` | where the binaries are copied (default `~/.local/bin`) |
//! | `GPN_AUTO_UPDATE` | `0`/`false`/`off` → `--apply` refuses to install |
//! | `GPN_UPDATE_RESTART_CMD` | command run after a successful swap (e.g. `systemctl --user restart gpn-fleet`). **Unset means no restart** — we print the command instead. |
//!
//! Restarting is never guessed: the wrong unit name takes the network down,
//! and only the operator knows whether this host is a `gpn_fleet`, a
//! `gpn-node`, or a hand-run process.
//!
//! ## What it touches on disk
//!
//! ```text
//! <state>/versions/<v>/          complete, stamp-verified release tree
//! <state>/current -> versions/<v>
//! <state>/current.previous       where current pointed before the flip
//! <state>/update-state.json      cached manifest result (shared with gpn_node)
//! <state>/pending-update.json    present only between staging and the flip
//! <target>/<each gpn* file>      replaced by `.name.tmp-<pid>` + rename
//! ```
//!
//! Nothing else is written. Removed binaries are not deleted (an operator's
//! extra tool may be one of them); stale versions are pruned, keeping `current`
//! and one rollback target.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use gpn_agent::update;
use serde::Serialize;

const EXIT_OK: u8 = 0;
const EXIT_ERROR: u8 = 1;
const EXIT_UNREACHABLE: u8 = 2;
const EXIT_UPDATE_AVAILABLE: u8 = 3;
/// `--confirm` could not see the expected version running before the deadline.
const EXIT_NOT_CONFIRMED: u8 = 4;

/// The binary this crate compiles. Reported as `current` only when the install
/// directory carries no readable build stamp (a hand-built dev tree).
const COMPILED_VERSION: &str = env!("CARGO_PKG_VERSION");

// ─────────────────────────────────────────────────────────────────────────────
// Options
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Check,
    Apply,
    /// Wait for the running node to report a specific version.
    Confirm,
    /// Flip `current` back to the version it pointed at before the last switch.
    Rollback,
    /// Repair `start-node.cmd` in place (Windows), so the next start runs the
    /// version the updater selected. Idempotent.
    RepairLauncher,
}

#[derive(Debug, Clone)]
struct Options {
    action: Action,
    json: bool,
    quiet: bool,
    state_dir: PathBuf,
    target_dir: PathBuf,
    manifest_url: String,
    /// `--force`: install even when the guard says the installed tree is a
    /// different (or newer) build of the same version, and ignore the
    /// once-per-boot attempt record.
    force: bool,
    /// `--expect V`: the version `--confirm` waits for.
    expect_version: Option<String>,
    /// `--timeout SECS`: how long `--confirm` waits (default 120).
    timeout_secs: u64,
    /// `--rpc URL`: where to ask the running node what version it is.
    rpc_url: String,
}

impl Options {
    fn from_env(args: impl Iterator<Item = String>) -> Result<Option<Options>, String> {
        let mut action: Option<Action> = None;
        let mut json = false;
        let mut quiet = false;
        let mut state_dir = None;
        let mut target_dir = None;
        let mut manifest_url = None;
        let mut force = false;
        let mut expect_version = None;
        let mut timeout_secs = 120u64;
        let mut rpc_url = default_rpc_url();

        let set_action = |a: Action, action: &mut Option<Action>| -> Result<(), String> {
            if action.is_some() {
                return Err("only one of --check, --apply, --confirm, --rollback, \
                     --repair-launcher may be given"
                    .into());
            }
            *action = Some(a);
            Ok(())
        };

        let mut it = args.peekable();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--check" => set_action(Action::Check, &mut action)?,
                "--apply" => set_action(Action::Apply, &mut action)?,
                "--confirm" => set_action(Action::Confirm, &mut action)?,
                "--rollback" => set_action(Action::Rollback, &mut action)?,
                "--repair-launcher" => set_action(Action::RepairLauncher, &mut action)?,
                "--expect" => expect_version = Some(it.next().ok_or("--expect needs a version")?),
                "--timeout" => {
                    let v = it.next().ok_or("--timeout needs seconds")?;
                    timeout_secs = v
                        .parse::<u64>()
                        .map_err(|_| format!("--timeout {v:?} is not a number of seconds"))?
                        .clamp(1, 3600);
                }
                "--rpc" => rpc_url = it.next().ok_or("--rpc needs a URL")?,
                "--json" => json = true,
                "--quiet" | "-q" => quiet = true,
                "--state-dir" => {
                    state_dir = Some(PathBuf::from(
                        it.next().ok_or("--state-dir needs a directory")?,
                    ))
                }
                "--target-dir" => {
                    target_dir = Some(PathBuf::from(
                        it.next().ok_or("--target-dir needs a directory")?,
                    ))
                }
                "--force" => force = true,
                "--url" => manifest_url = Some(it.next().ok_or("--url needs a URL")?),
                "-h" | "--help" => {
                    print_help();
                    return Ok(None);
                }
                "-V" | "--version" => {
                    println!("gpn_update {COMPILED_VERSION}");
                    return Ok(None);
                }
                other => return Err(format!("unknown argument {other:?} (try --help)")),
            }
        }

        let action = action.unwrap_or(Action::Check);
        if action == Action::Confirm && expect_version.is_none() {
            return Err("--confirm needs --expect <version>".into());
        }

        Ok(Some(Options {
            action,
            json,
            quiet,
            state_dir: state_dir.unwrap_or_else(default_state_dir),
            target_dir: target_dir.unwrap_or_else(default_target_dir),
            manifest_url: manifest_url.unwrap_or_else(default_manifest_url),
            force,
            expect_version,
            timeout_secs,
            rpc_url,
        }))
    }
}

fn print_help() {
    println!(
        "\
gpn_update {COMPILED_VERSION} — verify and install signed GPN releases

USAGE:
  gpn_update [--check|--apply|--confirm|--rollback|--repair-launcher]
             [--json] [--quiet] [--force]
             [--state-dir DIR] [--target-dir DIR] [--url URL]
             [--expect VERSION] [--timeout SECS] [--rpc URL]

  --check        fetch + verify the manifest, print current vs latest.
                 Downloads nothing. Exit 3 when a newer release exists.
  --apply        download, verify, extract, flip `current`, copy the binaries
                 into the target dir, prune old versions. When a restart
                 command is configured it also restarts, CONFIRMS the running
                 node reports the new version, and rolls back if it does not.
  --confirm      wait (up to --timeout, default 120 s) for the running node to
                 report --expect <version>. Exit 0 confirmed, 4 not confirmed.
  --rollback     flip `current` back to `current.previous` (one step).
  --repair-launcher  rewrite start-node.cmd so the next start runs the
                 versioned tree the updater selects (Windows; idempotent).
  --force        install even when the install dir already runs a different
                 build of the same version, or a newer one (refused by default:
                 a version comparison cannot see a locally deployed HEAD).
                 Also ignores the once-per-boot attempt record.
  --json         one JSON object on stdout (machine-readable).
  --quiet        no stdout noise (the systemd timer uses this).
  --state-dir    where versions/ + current live (default {})
  --target-dir   where the binaries are installed (default {})
  --url          manifest URL (default {})
  --expect       the version --confirm waits for
  --timeout      --confirm's deadline in seconds (default 120)
  --rpc          JSON-RPC endpoint of the running node (default {})

EXIT CODES: 0 up to date / applied / confirmed, 1 error, 2 manifest unreachable,
            3 newer verified release available (or not applied by policy),
            4 --confirm did not see the expected version running

ENV: GPN_AUTO_UPDATE=0 disables --apply.
     GPN_UPDATE_RESTART_CMD is run after a successful swap; when unset the
     command to run is printed instead (nothing is restarted by default).
     GPN_BOOT_ID identifies the current boot for the once-per-release attempt
     record (Windows passes it; Linux defaults to the kernel boot id).",
        default_state_dir().display(),
        default_target_dir().display(),
        update::MANIFEST_URL,
        default_rpc_url(),
    );
}

/// Where to ask the running node what version it is. The chain node's HTTP
/// JSON-RPC (`system_version`), which is the same port `--rpc-port` configures.
fn default_rpc_url() -> String {
    non_empty_env("GPN_RPC_URL")
        .or_else(|| non_empty_env("GPN_RPC_PORT").map(|p| format!("http://127.0.0.1:{p}")))
        .unwrap_or_else(|| "http://127.0.0.1:9944".to_string())
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn default_state_dir() -> PathBuf {
    if let Some(v) = non_empty_env("GPN_UPDATE_DIR") {
        return PathBuf::from(v);
    }
    if let Some(prefix) = non_empty_env("GPN_PREFIX") {
        return PathBuf::from(prefix).join("update");
    }
    home_dir().join(".gpn").join("update")
}

fn default_target_dir() -> PathBuf {
    non_empty_env("GPN_UPDATE_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".local").join("bin"))
}

fn default_manifest_url() -> String {
    non_empty_env("GPN_UPDATE_URL").unwrap_or_else(|| update::MANIFEST_URL.to_string())
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

/// The key the manifest must verify against.
///
/// **Release builds always use the compiled-in [`update::RELEASE_PUBKEY_HEX`].**
/// Debug builds additionally accept `GPN_UPDATE_PUBKEY`, because the release
/// private key is not (and must not be) in the tree, so the CLI tests could not
/// otherwise sign a manifest that verifies. `cfg(debug_assertions)` is off in
/// every shipped `--release` artifact, so this is test scaffolding that cannot
/// weaken a release binary.
fn trusted_pubkey() -> String {
    #[cfg(debug_assertions)]
    {
        if let Some(k) = non_empty_env("GPN_UPDATE_PUBKEY") {
            return k;
        }
    }
    update::RELEASE_PUBKEY_HEX.to_string()
}

// ─────────────────────────────────────────────────────────────────────────────
// Report (human + JSON)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Default, Serialize)]
struct Report {
    mode: &'static str,
    manifest_url: String,
    state_dir: String,
    target_dir: String,
    /// Version found in the install dir (build stamp), else this build's.
    current: String,
    current_source: String,
    latest: Option<String>,
    min_version: Option<String>,
    published_at: Option<String>,
    update_available: bool,
    upgrade_required: bool,
    layout: Option<String>,
    asset_url: Option<String>,
    asset_size: Option<u64>,
    asset_sha256: Option<String>,
    applied: bool,
    /// Binaries copied into the install dir, by name.
    installed_files: Vec<String>,
    versions_dir: Option<String>,
    previous: Option<String>,
    restart_command: Option<String>,
    restart_ran: Option<bool>,
    /// `--confirm`: the version the running node actually reported.
    confirmed_version: Option<String>,
    /// `--rollback`: the version `current` was flipped back to.
    rolled_back_to: Option<String>,
    /// Why an install was skipped by the once-per-boot attempt record.
    attempt_blocked: Option<String>,
    error: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Errors
// ─────────────────────────────────────────────────────────────────────────────

/// Distinguishes "could not reach the manifest" (exit 2) from a verification
/// or local failure (exit 1).
enum Failure {
    Unreachable(String),
    Verify(String),
}

impl Failure {
    fn exit_code(&self) -> u8 {
        match self {
            Failure::Unreachable(_) => EXIT_UNREACHABLE,
            Failure::Verify(_) => EXIT_ERROR,
        }
    }

    fn message(&self) -> &str {
        match self {
            Failure::Unreachable(m) | Failure::Verify(m) => m,
        }
    }
}

fn fetch(url: &str) -> Result<Vec<u8>, Failure> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| Failure::Unreachable(format!("http client: {e}")))?;
    let resp = client
        .get(url)
        .send()
        .map_err(|e| Failure::Unreachable(format!("{url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(Failure::Unreachable(format!(
            "{url} -> HTTP {}",
            resp.status()
        )));
    }
    resp.bytes()
        .map(|b| b.to_vec())
        .map_err(|e| Failure::Unreachable(format!("{url}: {e}")))
}

fn fetch_verified_manifest(url: &str) -> Result<update::ReleaseManifest, Failure> {
    let body = fetch(url)?;
    match update::verify_manifest(&body, &trusted_pubkey()) {
        Ok(m) => Ok(m),
        // A body that is not a manifest at all is a *source* problem (a captive
        // portal, an HTML error page, a truncated transfer): exit 2, the same
        // as unreachable. A well-formed manifest that fails authentication is
        // an attack/integrity problem: exit 1.
        Err(update::UpdateError::BadJson(e)) => Err(Failure::Unreachable(format!(
            "{url} did not return a release manifest: {e}"
        ))),
        Err(e) => Err(Failure::Verify(format!("manifest rejected: {e}"))),
    }
}

/// Attempts for one download (the first try plus its retries).
///
/// Three is deliberate: the failure this exists for (a GitHub-wide `504` on the
/// release asset, 2026-11-08 00:55 PT) lasted seconds, so a short burst of
/// retries rides it out, while a genuinely down mirror still fails the unit in
/// under ten seconds instead of hanging a systemd timer.
const DOWNLOAD_ATTEMPTS: u32 = 3;

/// First backoff, doubled per retry (2 s, then 4 s). `GPN_UPDATE_RETRY_BASE_MS`
/// exists so the end-to-end tests do not spend six seconds asleep.
const DOWNLOAD_BACKOFF_MS: u64 = 2_000;

fn retry_base_ms() -> u64 {
    std::env::var("GPN_UPDATE_RETRY_BASE_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DOWNLOAD_BACKOFF_MS)
}

/// Backoff before the next attempt, after `attempt` (1-based) has failed.
fn retry_backoff_ms(attempt: u32, base_ms: u64) -> u64 {
    let shift = attempt.saturating_sub(1).min(6);
    base_ms.saturating_mul(1u64 << shift)
}

/// The HTTP status this module embedded in a download error, if any.
fn http_status_in(msg: &str) -> Option<u16> {
    // Only the shape this module produces: "download <url> -> HTTP 504".
    let i = msg.rfind("-> HTTP ")?;
    msg[i + "-> HTTP ".len()..]
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Is a failed attempt worth repeating?
///
/// **5xx only** for anything the mirror answered: a server error is the mirror
/// having a bad moment, while any 4xx (including 404 and 429) is the mirror
/// telling us *this request* is wrong — repeating it is how a broken or
/// mis-pointed mirror becomes a retry storm against a rate-limited host. A
/// transport failure with no status (timeout, connection reset/closed/aborted,
/// truncated body) never reached an answer at all, so it is retried too.
///
/// **Hash and size mismatches are never retried here by construction**: they are
/// checked after the bytes are staged (`update::stage_bundle_file` /
/// `update::verify_bytes`), outside this helper. Retrying a corrupt transfer
/// would only multiply the bandwidth of an already-suspect asset and would hide
/// the real signal (a wrong or tampered asset) behind three identical attempts.
fn download_failure_is_transient(msg: &str) -> bool {
    if let Some(code) = http_status_in(msg) {
        return (500..=599).contains(&code);
    }
    let m = msg.to_ascii_lowercase();
    const TRANSIENT: [&str; 9] = [
        "timed out",
        "timeout",
        "connection reset",
        "connection closed",
        "connection aborted",
        "connection refused",
        "broken pipe",
        "unexpected eof",
        "error decoding response body",
    ];
    TRANSIENT.iter().any(|needle| m.contains(needle))
}

/// Run a network step with up to [`DOWNLOAD_ATTEMPTS`] tries, backing off
/// exponentially, and give up on the first error that is not transient.
fn with_transient_retry<T>(
    what: &str,
    attempt: impl FnMut() -> Result<T, String>,
) -> Result<T, String> {
    with_transient_retry_base(what, retry_base_ms(), attempt)
}

fn with_transient_retry_base<T>(
    what: &str,
    base_ms: u64,
    mut attempt: impl FnMut() -> Result<T, String>,
) -> Result<T, String> {
    let mut tries = 1u32;
    loop {
        match attempt() {
            Ok(v) => {
                if tries > 1 {
                    eprintln!(
                        "gpn_update: {what} succeeded on attempt {tries}/{DOWNLOAD_ATTEMPTS}"
                    );
                }
                return Ok(v);
            }
            Err(e) => {
                if tries >= DOWNLOAD_ATTEMPTS || !download_failure_is_transient(&e) {
                    return Err(if tries > 1 {
                        format!("{what} failed after {tries} attempts; last error: {e}")
                    } else {
                        e
                    });
                }
                let wait = retry_backoff_ms(tries, base_ms);
                eprintln!(
                    "gpn_update: {what} attempt {tries}/{DOWNLOAD_ATTEMPTS} failed: {e} \
                     — retrying in {:.1}s",
                    wait as f64 / 1000.0
                );
                std::thread::sleep(Duration::from_millis(wait));
                tries += 1;
            }
        }
    }
}

fn download_to_file(url: &str, dest: &Path, timeout: Duration) -> Result<u64, String> {
    use std::io::Read;
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let mut resp = client
        .get(url)
        .send()
        .map_err(|e| format!("download {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download {url} -> HTTP {}", resp.status()));
    }
    let mut f = std::fs::File::create(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let mut buf = [0u8; 256 * 1024];
    let mut total = 0u64;
    loop {
        let n = resp
            .read(&mut buf)
            .map_err(|e| format!("download body: {e}"))?;
        if n == 0 {
            break;
        }
        f.write_all(&buf[..n])
            .map_err(|e| format!("{}: {e}", dest.display()))?;
        total += n as u64;
    }
    f.sync_all().ok();
    Ok(total)
}

fn fmt_mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

/// The version in the install dir, and where it came from.
///
/// Comparing against the *installed* supervisor instead of this binary's
/// `CARGO_PKG_VERSION` matters: an updater that is itself one release ahead
/// (it was shipped inside the bundle) must still see the box as "0.3.11" until
/// the copy step runs.
fn installed_version(o: &Options) -> (String, String) {
    // Two sources of truth, and they can disagree:
    //  * `current` is what the launcher on a Windows box will run next;
    //  * the install dir is what runs on Linux *and* what `--apply` copies into.
    //
    // A mid-copy failure leaves `current` on the new version and the install dir
    // mixed, so answering "up to date" from `current` alone would hide a broken
    // install forever. Report the LOWER version when they disagree: that can
    // only cause work (a re-apply), never hide some.
    let from_stamp = [update::node_bin_name(), update::rust_node_bin_name()]
        .iter()
        .find_map(|name| {
            let p = o.target_dir.join(name);
            update::read_build_stamp(&p).ok().flatten().map(|v| (v, p))
        });
    let from_current = update::installed_version(&o.state_dir);

    match (from_current, from_stamp) {
        (Some(cur), Some((stamp, path))) => match update::version_cmp(&cur, &stamp) {
            Some(std::cmp::Ordering::Equal) => {
                (cur, format!("current -> {}", o.state_dir.display()))
            }
            Some(std::cmp::Ordering::Less) => (
                cur,
                format!(
                    "current -> (lower than the install dir's {stamp} at {})",
                    path.display()
                ),
            ),
            _ => (
                stamp,
                format!(
                    "{} (lower than current -> {cur}; the install and the version \
                     tree disagree, so an apply will repair them)",
                    path.display()
                ),
            ),
        },
        (Some(cur), None) => (cur, format!("current -> {}", o.state_dir.display())),
        (None, Some((stamp, path))) => (stamp, path.display().to_string()),
        (None, None) => (
            COMPILED_VERSION.to_string(),
            "compiled-in (no build stamp in the install dir)".to_string(),
        ),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// --check
// ─────────────────────────────────────────────────────────────────────────────

fn run_check(o: &Options, report: &mut Report) -> u8 {
    let m = match fetch_verified_manifest(&o.manifest_url) {
        Ok(m) => m,
        Err(e) => {
            report.error = Some(e.message().to_string());
            emit_error(o, report, e.message());
            return e.exit_code();
        }
    };

    report.latest = Some(m.version.clone());
    report.min_version = Some(m.min_version.clone());
    report.published_at = Some(m.published_at.clone());

    let asset = match update::select_asset(&m, update::platform_os(), update::platform_arch()) {
        Ok(a) => a,
        Err(e) => {
            let msg = format!("verified manifest is unusable: {e}");
            report.error = Some(msg.clone());
            emit_error(o, report, &msg);
            return EXIT_ERROR;
        }
    };

    let newer = update::is_newer(&m.version, &report.current);
    let required = update::min_version_decision(&report.current, &m.min_version)
        == update::Support::UpgradeRequired;
    report.update_available = newer;
    report.upgrade_required = required;
    report.layout = Some(update::asset_layout(asset));
    report.asset_url = Some(asset.url.clone());
    report.asset_size = Some(asset.size);
    report.asset_sha256 = Some(asset.sha256.clone());

    if !o.quiet && !o.json {
        println!("gpn_update: manifest verified ({})", o.manifest_url);
        println!(
            "  current      {}   {}",
            report.current, report.current_source
        );
        println!(
            "  latest       {}{}",
            m.version,
            if m.published_at.is_empty() {
                String::new()
            } else {
                format!("   published {}", m.published_at)
            }
        );
        println!(
            "  min_version  {}   ({})",
            m.min_version,
            if required {
                "UPGRADE REQUIRED"
            } else {
                "supported"
            }
        );
        println!(
            "  asset        {}/{} layout={} {} sha256={}",
            asset.os,
            asset.arch,
            update::asset_layout(asset),
            fmt_mb(asset.size),
            short(&asset.sha256)
        );
        if newer {
            println!(
                "  -> update available: {} (run `gpn_update --apply`)",
                m.version
            );
        } else {
            println!("  -> up to date ({})", report.current);
        }
    }
    if o.json {
        emit_json(report);
    }

    if newer {
        EXIT_UPDATE_AVAILABLE
    } else if required {
        // The manifest says this build may not participate, yet it publishes no
        // newer version — a publisher error we must not paper over.
        let msg = format!(
            "manifest requires >= {}, but its latest release is {} — refusing to \
             call this host up to date",
            m.min_version, m.version
        );
        report.error = Some(msg.clone());
        emit_error(o, report, &msg);
        EXIT_ERROR
    } else {
        EXIT_OK
    }
}

fn short(hex: &str) -> &str {
    &hex[..hex.len().min(16)]
}

// ─────────────────────────────────────────────────────────────────────────────
// --apply
// ─────────────────────────────────────────────────────────────────────────────

/// Why an install did not happen.
///
/// `Refused` is a *policy* answer (the guard kept a locally deployed build in
/// place) and exits `3`, the same "a release exists but was not applied" code
/// the `GPN_AUTO_UPDATE=0` path uses — a timer fire that correctly declines is
/// not a failure. `Failed` is a real error (exit `1`).
enum InstallFailure {
    Refused(String),
    Failed(String),
}

impl InstallFailure {
    fn message(&self) -> &str {
        match self {
            InstallFailure::Refused(m) | InstallFailure::Failed(m) => m,
        }
    }

    fn exit_code(&self) -> u8 {
        match self {
            InstallFailure::Refused(_) => EXIT_UPDATE_AVAILABLE,
            InstallFailure::Failed(_) => EXIT_ERROR,
        }
    }
}

fn guard_failed(msg: String) -> InstallFailure {
    InstallFailure::Failed(msg)
}

/// Every stamped binary in the install dir, with its name.
///
/// All of them, not just the supervisor: the `#148` incident was a **mixed**
/// install dir — `gpn_node` was the old release (so the version comparison saw
/// "0.3.27 → 0.3.28, go ahead") while `gpn_scheduler` was a locally deployed
/// `HEAD` build of 0.3.28 that the release did not contain. Looking only at the
/// supervisor cannot see that.
fn installed_stamps(target: &Path) -> Vec<(String, update::BuildStamp)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(target) else {
        return out;
    };
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| is_installable_binary(n))
        .collect();
    names.sort();
    for name in names {
        if let Ok(Some(stamp)) = update::read_build_stamp_full(&target.join(&name)) {
            out.push((name, stamp));
        }
    }
    out
}

/// Refuse to replace a build the release does not contain.
///
/// Runs **after** the downloaded tree is verified and **before** `current` is
/// flipped or any file in the install dir is touched, so a refusal leaves the
/// host exactly as it was.
fn guard_install(o: &Options, staged: &Path) -> Result<(), InstallFailure> {
    let candidate = update::read_build_stamp_full(staged)
        .map_err(|e| InstallFailure::Failed(format!("{}: {e}", staged.display())))?;
    for (name, installed) in installed_stamps(&o.target_dir) {
        if let update::InstallVerdict::Refuse(msg) =
            update::install_guard(Some(&installed), candidate.as_ref(), o.force)
        {
            return Err(InstallFailure::Refused(format!("{name}: {msg}")));
        }
    }
    Ok(())
}

fn run_apply(o: &Options, report: &mut Report) -> u8 {
    let m = match fetch_verified_manifest(&o.manifest_url) {
        Ok(m) => m,
        Err(e) => {
            report.error = Some(e.message().to_string());
            emit_error(o, report, e.message());
            return e.exit_code();
        }
    };
    report.latest = Some(m.version.clone());
    report.min_version = Some(m.min_version.clone());
    report.published_at = Some(m.published_at.clone());

    let newer = update::is_newer(&m.version, &report.current);
    let required = update::min_version_decision(&report.current, &m.min_version)
        == update::Support::UpgradeRequired;
    report.update_available = newer;
    report.upgrade_required = required;

    if !newer {
        if required {
            let msg = format!(
                "manifest requires >= {} but its latest release is {} — there is nothing \
                 to install that satisfies the cutoff",
                m.min_version, m.version
            );
            report.error = Some(msg.clone());
            emit_error(o, report, &msg);
            return EXIT_ERROR;
        }
        if o.json {
            emit_json(report);
        } else if !o.quiet {
            println!("gpn_update: up to date ({})", report.current);
        }
        return EXIT_OK;
    }

    // Policy check before any download: GPN_AUTO_UPDATE=0 means "tell me, do
    // not install". The manifest is already verified, so `3` is a truthful
    // "newer release exists, not applied".
    if !update::auto_update_enabled(non_empty_env("GPN_AUTO_UPDATE").as_deref()) {
        let msg = format!(
            "{} is available, but GPN_AUTO_UPDATE=0 — refusing to install. \
             Re-run with GPN_AUTO_UPDATE=1 (or unset) to apply.",
            m.version
        );
        report.error = Some(msg.clone());
        emit_error(o, report, &msg);
        return EXIT_UPDATE_AVAILABLE;
    }

    let asset = match update::select_asset(&m, update::platform_os(), update::platform_arch()) {
        Ok(a) => a,
        Err(e) => {
            let msg = format!("verified manifest is unusable: {e}");
            report.error = Some(msg.clone());
            emit_error(o, report, &msg);
            return EXIT_ERROR;
        }
    };

    // Once per version per boot. A release that installs but does not come up
    // must not be retried every hour on the same boot: that is a restart loop
    // with a 300 MB download attached. The record is written before the install
    // and updated with its outcome, so a crash mid-update also counts.
    let boot = update::boot_id();
    if !o.force {
        if let Some(prev) = update::attempt_for_this_boot(&o.state_dir, &m.version, &boot) {
            if update::attempt_blocks(&prev) {
                let msg = format!(
                    "{} was already attempted on this boot ({}, at {} unix) and did not confirm \
                     — not retrying. The previous build stays running. Rebooting is the \
                     documented way to re-arm; `--force` overrides this.",
                    m.version, prev.outcome, prev.at
                );
                report.attempt_blocked = Some(msg.clone());
                report.error = Some(msg.clone());
                emit_error(o, report, &msg);
                return EXIT_UPDATE_AVAILABLE;
            }
        }
    }

    let bundle = update::is_bundle(asset);
    report.layout = Some(update::asset_layout(asset));
    report.asset_url = Some(asset.url.clone());
    report.asset_size = Some(asset.size);
    report.asset_sha256 = Some(asset.sha256.clone());

    match install(o, report, &m, asset, bundle) {
        Ok(()) => {}
        Err(fail) => {
            let msg = fail.message().to_string();
            report.error = Some(msg.clone());
            emit_error(o, report, &msg);
            return fail.exit_code();
        }
    }

    report.applied = true;
    // State was swapped; now publish the binaries the fleet actually executes.
    let source = if bundle {
        update::version_dir(&o.state_dir, &m.version)
    } else {
        o.state_dir.join("single").join(&m.version)
    };
    match copy_binaries(&source, &o.target_dir) {
        Ok(copied) => {
            report.installed_files = copied;
        }
        Err(msg) => {
            let full = format!(
                "{msg}\n  The version tree and `current` were updated, but the install dir \
                 is now MIXED. Re-run `gpn_update --apply` (it reuses the verified tree) \
                 before restarting anything."
            );
            report.error = Some(full.clone());
            emit_error(o, report, &full);
            return EXIT_ERROR;
        }
    }

    if !o.quiet && !o.json {
        println!("  installed {} at {}", m.version, source.display());
        println!(
            "  current -> versions/{}   previous: {}",
            m.version,
            report.previous.as_deref().unwrap_or("(none)")
        );
        println!(
            "  copied {} binaries to {}",
            report.installed_files.len(),
            o.target_dir.display()
        );
    }

    // The install is done; from here on the only question that matters is
    // whether the *running* process becomes the new build.
    let _ = update::record_attempt(&o.state_dir, &m.version, &boot, update::OUTCOME_STARTED);

    match restart(o, report) {
        Ok(()) => {
            if report.restart_ran == Some(true) {
                match wait_for_version(o, &m.version) {
                    Ok(got) => {
                        report.confirmed_version = Some(got.clone());
                        let _ = update::record_attempt(
                            &o.state_dir,
                            &m.version,
                            &boot,
                            update::OUTCOME_CONFIRMED,
                        );
                        if !o.quiet && !o.json {
                            println!("  confirmed: {got} is running");
                        }
                    }
                    Err(reason) => {
                        return roll_back_after_failed_confirm(
                            o, report, &m.version, &boot, &reason,
                        )
                    }
                }
            } else {
                // No restart command: the new binaries are on disk and the old
                // ones are still running. "Installed" is the honest outcome —
                // not a confirmation, and not a failure.
                let _ = update::record_attempt(
                    &o.state_dir,
                    &m.version,
                    &boot,
                    update::OUTCOME_INSTALLED,
                );
            }
            if o.json {
                emit_json(report);
            }
            EXIT_OK
        }
        Err(msg) => {
            let _ = update::record_attempt(&o.state_dir, &m.version, &boot, update::OUTCOME_FAILED);
            report.error = Some(msg.clone());
            emit_error(o, report, &msg);
            EXIT_ERROR
        }
    }
}

/// The new build did not come up: put the previous one back, start it, and
/// record the failure so this boot does not try the same release again.
///
/// The node is never left down: if the rollback itself fails we still run the
/// restart command, because a box running the previous build is the floor, and
/// a box with nothing running is worse than the bug we are reacting to.
fn roll_back_after_failed_confirm(
    o: &Options,
    report: &mut Report,
    version: &str,
    boot: &str,
    reason: &str,
) -> u8 {
    eprintln!("gpn_update: NOT CONFIRMED — {reason}");
    let rolled = update::rollback_bundle(&o.state_dir);
    let (msg, prev_label) = match rolled {
        Ok(Some(prev)) => {
            // A rollback that leaves the install dir on the new binaries is not
            // a rollback: on Linux (and on any launcher that runs `bin`) the
            // restart would start the build that just failed. Best effort — the
            // `current` flip is what matters on Windows, where the launcher
            // runs the versioned tree.
            if let Err(e) = copy_binaries(&prev, &o.target_dir) {
                eprintln!(
                    "gpn_update: `current` is back on {}, but refreshing {} failed: {e}",
                    prev.display(),
                    o.target_dir.display()
                );
            }
            let label = prev
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| prev.display().to_string());
            (
                format!("{version} did not come up ({reason}); rolled `current` back to {label}"),
                Some(label),
            )
        }
        Ok(None) => (
            format!(
                "{version} did not come up ({reason}); there is no rollback target, so the \
                 previous build is still the one on disk"
            ),
            None,
        ),
        Err(e) => (
            format!("{version} did not come up ({reason}); the rollback also failed: {e}"),
            None,
        ),
    };

    // Restart whatever is now selected. Never end with the node down.
    match restart(o, report) {
        Ok(()) => {}
        Err(e) => eprintln!("gpn_update: could not restart after the rollback: {e}"),
    }

    let _ = update::record_attempt(&o.state_dir, version, boot, update::OUTCOME_ROLLED_BACK);
    report.rolled_back_to = prev_label;
    report.error = Some(msg.clone());
    emit_error(o, report, &msg);
    EXIT_ERROR
}

// ─────────────────────────────────────────────────────────────────────────────
// --confirm / --rollback / --repair-launcher
// ─────────────────────────────────────────────────────────────────────────────

/// Ask the running node what version it is, via the same JSON-RPC the operator
/// and the dashboard use.
///
/// Short per-request timeout: `--confirm` polls, so one slow answer must not
/// consume the whole deadline.
fn rpc_system_version(url: &str, timeout: Duration) -> Result<String, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "system_version", "params": []
    });
    let bytes = serde_json::to_vec(&body).map_err(|e| e.to_string())?;
    let resp = client
        .post(url)
        .header("content-type", "application/json")
        .body(bytes)
        .send()
        .map_err(|e| format!("{url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("{url} -> HTTP {}", resp.status()));
    }
    let v: serde_json::Value = resp
        .json()
        .map_err(|e| format!("{url}: response is not JSON: {e}"))?;
    if let Some(err) = v.get("error") {
        return Err(format!("{url}: RPC error {err}"));
    }
    v.get("result")
        .and_then(|r| r.as_str())
        .map(|s| s.trim().to_string())
        .ok_or_else(|| format!("{url}: no string `result` in {v}"))
}

/// Poll the running node until it reports `version`, or the deadline passes.
fn wait_for_version(o: &Options, version: &str) -> Result<String, String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(o.timeout_secs);
    let mut last: Option<String> = None;
    loop {
        match rpc_system_version(&o.rpc_url, Duration::from_secs(3)) {
            Ok(got) if update::version_matches(version, &got) => return Ok(got),
            Ok(got) => last = Some(format!("the node reports {got}")),
            Err(e) => last = Some(e),
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    Err(format!(
        "{version} is not running after {} s: {} (asked {})",
        o.timeout_secs,
        last.unwrap_or_else(|| "no answer yet".to_string()),
        o.rpc_url
    ))
}

/// Wait until the *running* node reports `expect`.
fn run_confirm(o: &Options, report: &mut Report) -> u8 {
    let expect = o.expect_version.as_deref().unwrap_or("").trim().to_string();
    if expect.is_empty() {
        report.error = Some("--confirm needs --expect <version>".into());
        emit_error(o, report, "--confirm needs --expect <version>");
        return EXIT_ERROR;
    }
    match wait_for_version(o, &expect) {
        Ok(got) => {
            report.confirmed_version = Some(got.clone());
            if o.json {
                emit_json(report);
            } else if !o.quiet {
                println!("gpn_update: confirmed {got} is running ({})", o.rpc_url);
            }
            EXIT_OK
        }
        Err(msg) => {
            report.error = Some(msg.clone());
            emit_error(o, report, &msg);
            EXIT_NOT_CONFIRMED
        }
    }
}

/// Flip `current` back one step. The caller restarts whatever needs restarting.
fn run_rollback(o: &Options, report: &mut Report) -> u8 {
    // The version being abandoned: recorded as a failed attempt on this boot so
    // the next cycle does not walk into the same release again.
    let from = update::installed_version(&o.state_dir);
    match update::rollback_bundle(&o.state_dir) {
        Ok(Some(prev)) => {
            // Keep the install dir in step with `current`. On Linux the unit
            // execs `<target>/gpn-node`, so a rollback that only flipped the
            // link would restart the NEW build and undo itself.
            match copy_binaries(&prev, &o.target_dir) {
                Ok(n) => {
                    if !o.quiet {
                        println!(
                            "  restored {} binaries in {}",
                            n.len(),
                            o.target_dir.display()
                        );
                    }
                }
                Err(e) => eprintln!(
                    "gpn_update: `current` is back on the previous version, but refreshing {} \
                     failed: {e}",
                    o.target_dir.display()
                ),
            }
            let v = prev
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| prev.display().to_string());
            report.rolled_back_to = Some(v.clone());
            if let Some(from) = from.as_deref() {
                let _ = update::record_attempt(
                    &o.state_dir,
                    from,
                    &update::boot_id(),
                    update::OUTCOME_ROLLED_BACK,
                );
            }
            if o.json {
                emit_json(report);
            } else if !o.quiet {
                println!("gpn_update: rolled back to {v} ({})", prev.display());
            }
            EXIT_OK
        }
        Ok(None) => {
            let msg = format!(
                "no rollback target: {} does not exist (nothing to roll back to)",
                update::previous_pointer(&o.state_dir).display()
            );
            report.error = Some(msg.clone());
            emit_error(o, report, &msg);
            EXIT_ERROR
        }
        Err(e) => {
            let msg = format!("rollback failed: {e}");
            report.error = Some(msg.clone());
            emit_error(o, report, &msg);
            EXIT_ERROR
        }
    }
}

/// Rewrite the launcher so the next start runs the selected version tree.
fn run_repair_launcher(o: &Options) -> u8 {
    // `--target-dir` is the install dir's `bin`; the launcher lives one up.
    let install = o
        .target_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| o.target_dir.clone());
    match update::repair_launcher_in_dir(&install) {
        Some(note) => {
            if !o.quiet {
                println!("gpn_update: {note}");
            }
            EXIT_OK
        }
        None => {
            if !o.quiet {
                println!(
                    "gpn_update: {} needs no repair (or does not exist on this platform)",
                    install.join("start-node.cmd").display()
                );
            }
            EXIT_OK
        }
    }
}

/// Stage (or reuse) the verified tree and flip `current`.
fn install(
    o: &Options,
    report: &mut Report,
    m: &update::ReleaseManifest,
    asset: &update::ReleaseAsset,
    bundle: bool,
) -> Result<(), InstallFailure> {
    let state = &o.state_dir;
    std::fs::create_dir_all(state)
        .map_err(|e| guard_failed(format!("{}: {e}", state.display())))?;
    report.versions_dir = Some(update::versions_root(state).display().to_string());

    if !bundle {
        return install_single(o, report, m, asset);
    }

    let vdir = update::version_dir(state, &m.version);
    let already = update::bundle_installed(state, &m.version);
    if already {
        // A previous run downloaded and staged this exact version but died in
        // the copy step. Do not pull 300 MB again: re-verify what is on disk.
        update::verify_bundle_stamps(&vdir, &m.version).map_err(|e| guard_failed(e.to_string()))?;
        update::verify_bundle_files(&vdir, &asset.files)
            .map_err(|e| guard_failed(e.to_string()))?;
        if !o.quiet && !o.json {
            println!(
                "gpn_update: {} is already staged at {} — reusing it",
                m.version,
                vdir.display()
            );
        }
    } else {
        if !o.quiet && !o.json {
            println!(
                "gpn_update: downloading {} ({}/{}, layout=bundle, {})",
                m.version,
                asset.os,
                asset.arch,
                fmt_mb(asset.size)
            );
        }
        let tmp = update::bundle_temp_path(state, &m.version, asset);
        let staged = with_transient_retry("download", || {
            download_to_file(&asset.url, &tmp, Duration::from_secs(3600)).inspect_err(|_| {
                // A half-written archive must not become the next attempt's
                // starting point (or be mistaken for a staged asset).
                let _ = std::fs::remove_file(&tmp);
            })
        })
        .and_then(|_| {
            update::stage_bundle_file(state, &m.version, asset, &tmp).map_err(|e| e.to_string())
        });
        let _ = std::fs::remove_file(&tmp);
        staged.map_err(InstallFailure::Failed)?;
    }

    // The tree is verified; now ask whether it may replace what is installed.
    // A "no" here leaves `current` and every file in the install dir untouched.
    guard_install(o, &update::node_bin(&vdir))?;

    // Flip `current`. The pending marker is written by stage_bundle_file; when
    // we reused an existing tree there is no marker, so activate it directly.
    let previous = update::current_version_dir(state);
    if previous.is_none() && report.current != m.version {
        // First update managed by this tool on this host: snapshot the binaries
        // the install dir is running *now* into `versions/<running>/`, so the
        // first update still has a rollback target.
        if let Some(seed) = seed_previous_from_target(state, &o.target_dir, &report.current) {
            let _ = update::record_previous(state, &seed);
        }
    }
    if update::read_pending(state).is_some() {
        match update::apply_pending(state, &std::env::current_exe().unwrap_or_default()) {
            Ok(Ok(Some(_))) | Ok(Ok(None)) => {}
            Ok(Err(deferred)) => {
                return Err(InstallFailure::Failed(format!(
                    "could not flip `current` to {}: {}",
                    m.version, deferred.reason
                )))
            }
            Err(e) => {
                return Err(InstallFailure::Failed(format!(
                    "could not flip `current`: {e}"
                )))
            }
        }
    } else if update::current_version_dir(state).as_deref() != Some(vdir.as_path()) {
        update::activate_version(state, &m.version)
            .map_err(|e| InstallFailure::Failed(format!("could not flip `current`: {e}")))?;
    }
    report.previous = update::previous_pointer_label(state);
    Ok(())
}

/// Copy the currently-installed `gpn*` binaries into `versions/<version>/`,
/// provided they still carry a consistent build stamp.
///
/// This is what gives a `gpn_fleet` host — which never had an update state dir —
/// a rollback target on its very first `--apply`. If anything about the install
/// dir does not verify we quietly return `None`: seeding is a bonus, never a
/// reason to fail an update.
fn seed_previous_from_target(state: &Path, target: &Path, version: &str) -> Option<PathBuf> {
    update::parse_version(version)?;
    let vdir = update::version_dir(state, version);
    if vdir.is_dir() {
        return Some(vdir);
    }
    let root = update::versions_root(state);
    std::fs::create_dir_all(&root).ok()?;
    let tmp = root.join(format!(".{}.seed-{}", version, std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).ok()?;

    let mut copied = 0usize;
    let rd = std::fs::read_dir(target).ok()?;
    for e in rd.filter_map(|e| e.ok()) {
        let name = e.file_name().to_string_lossy().into_owned();
        if !is_installable_binary(&name) || !e.path().is_file() {
            continue;
        }
        if std::fs::copy(e.path(), tmp.join(&name)).is_err() {
            let _ = std::fs::remove_dir_all(&tmp);
            return None;
        }
        copied += 1;
    }
    if copied == 0 || update::verify_bundle_stamps(&tmp, version).is_err() {
        let _ = std::fs::remove_dir_all(&tmp);
        return None;
    }
    if std::fs::rename(&tmp, &vdir).is_err() {
        let _ = std::fs::remove_dir_all(&tmp);
        if !vdir.is_dir() {
            return None;
        }
    }
    Some(vdir)
}

/// Single-binary layout: fetch, verify, and drop the one file into
/// `<state>/single/<v>/` so the copy step has the same shape as a bundle.
fn install_single(
    o: &Options,
    report: &mut Report,
    m: &update::ReleaseManifest,
    asset: &update::ReleaseAsset,
) -> Result<(), InstallFailure> {
    let dir = o.state_dir.join("single").join(&m.version);
    let dest = dir.join(update::node_bin_name());
    if dest.is_file() {
        update::verify_file(&dest, asset).map_err(|e| guard_failed(e.to_string()))?;
    } else {
        if !o.quiet && !o.json {
            println!(
                "gpn_update: downloading {} ({})",
                m.version,
                fmt_mb(asset.size)
            );
        }
        std::fs::create_dir_all(&dir)
            .map_err(|e| guard_failed(format!("{}: {e}", dir.display())))?;
        // Buffered, but a single-layout asset is one binary (not a bundle).
        // The same transient-retry rule applies: a 5xx/timeout is the mirror
        // having a bad moment, a 4xx or a failed hash check is not.
        let body = with_transient_retry("download", || {
            fetch(&asset.url).map_err(|e| e.message().to_string())
        })
        .map_err(InstallFailure::Failed)?;
        update::verify_bytes(&body, asset).map_err(|e| guard_failed(e.to_string()))?;
        let tmp = dir.join(format!(
            ".{}.tmp-{}",
            update::node_bin_name(),
            std::process::id()
        ));
        std::fs::write(&tmp, &body).map_err(|e| guard_failed(format!("{}: {e}", tmp.display())))?;
        set_executable(&tmp).map_err(InstallFailure::Failed)?;
        std::fs::rename(&tmp, &dest)
            .map_err(|e| guard_failed(format!("{}: {e}", dest.display())))?;
        update::verify_file(&dest, asset).map_err(|e| guard_failed(e.to_string()))?;
    }
    guard_install(o, &dest)?;
    report.previous = None;
    Ok(())
}

/// Files in a version tree / install dir that are binaries to install.
///
/// Staged (`*.new`), rollback (`*.bak`) and in-flight (`.*.tmp-*`, `*.download`)
/// files must never be mistaken for release binaries — seeding a rollback tree
/// from the install dir would otherwise copy a half-applied staging file into
/// it and then into every future version.
fn is_installable_binary(name: &str) -> bool {
    name.starts_with("gpn")
        && !name.ends_with(update::STAGED_SUFFIX)
        && !name.ends_with(update::BACKUP_SUFFIX)
        && !name.contains(".tmp-")
        && !name.ends_with(".download")
}

/// Debug-only fault injection so a test can prove that a crash between two
/// per-file swaps leaves the untouched binary intact (never truncated).
#[cfg(debug_assertions)]
fn test_fail_after_copies() -> Option<usize> {
    non_empty_env("GPN_UPDATE_TEST_FAIL_AFTER_COPIES").and_then(|v| v.parse().ok())
}

#[cfg(not(debug_assertions))]
fn test_fail_after_copies() -> Option<usize> {
    None
}

/// Copy every `gpn*` file from a verified version tree into the install dir,
/// **one atomic rename at a time**.
///
/// Each file is written to `.<name>.tmp-<pid>` in the target directory, made
/// executable, and renamed over the destination. On Unix a rename over a
/// running executable is atomic and works while the old image is mapped; a
/// crash therefore leaves either the old file or the complete new one, never a
/// half-written executable.
fn copy_binaries(source: &Path, target: &Path) -> Result<Vec<String>, String> {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let rd = std::fs::read_dir(source).map_err(|e| format!("{}: {e}", source.display()))?;
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| is_installable_binary(n))
        .collect();
    if names.is_empty() {
        return Err(format!(
            "{} contains no gpn* binaries — refusing to report success",
            source.display()
        ));
    }
    names.sort();

    std::fs::create_dir_all(target).map_err(|e| format!("{}: {e}", target.display()))?;

    let fail_after = test_fail_after_copies();
    let mut done: Vec<String> = Vec::new();
    for name in &names {
        if let Some(n) = fail_after {
            if done.len() >= n {
                return Err(format!(
                    "injected failure after {} of {} copies (GPN_UPDATE_TEST_FAIL_AFTER_COPIES)",
                    done.len(),
                    names.len()
                ));
            }
        }
        let src = source.join(name);
        let dst = target.join(name);
        let tmp = target.join(format!(".{name}.tmp-{}", std::process::id()));
        if let Err(e) = std::fs::copy(&src, &tmp) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("{} -> {}: {e}", src.display(), dst.display()));
        }
        // Unix: mark executable before the rename. Windows has no mode bits --
        // the copied .exe is already runnable, and an unconditional from_mode()
        // here is what broke the Windows build on 2026-11-03.
        #[cfg(unix)]
        if let Err(e) = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("chmod {}: {e}", tmp.display()));
        }
        #[cfg(not(unix))]
        let _ = &tmp;
        if let Err(e) = std::fs::rename(&tmp, &dst) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!(
                "{}: {e}{}",
                dst.display(),
                if cfg!(windows) {
                    " (stop the node/supervisor and retry — Windows locks a running .exe)"
                } else {
                    ""
                }
            ));
        }
        done.push(name.clone());
    }
    Ok(done)
}

fn set_executable(p: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod {}: {e}", p.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = p;
    }
    Ok(())
}

/// Run `GPN_UPDATE_RESTART_CMD` if the operator set one; otherwise print the
/// command they should run. **Never restart by default** — guessing the unit
/// name is how an updater takes the network down.
fn restart(o: &Options, report: &mut Report) -> Result<(), String> {
    let cmd = non_empty_env("GPN_UPDATE_RESTART_CMD");
    match cmd {
        Some(cmd) => {
            report.restart_command = Some(cmd.clone());
            if !o.quiet && !o.json {
                println!("  restart: {cmd}");
            }
            let status = if cfg!(windows) {
                std::process::Command::new("cmd")
                    .args(["/C", &cmd])
                    .status()
            } else {
                std::process::Command::new("sh")
                    .arg("-c")
                    .arg(&cmd)
                    .status()
            };
            match status {
                Ok(s) if s.success() => {
                    report.restart_ran = Some(true);
                    Ok(())
                }
                Ok(s) => {
                    report.restart_ran = Some(false);
                    Err(format!(
                        "binaries installed, but GPN_UPDATE_RESTART_CMD exited {s}: {cmd}"
                    ))
                }
                Err(e) => {
                    report.restart_ran = Some(false);
                    Err(format!(
                        "binaries installed, but GPN_UPDATE_RESTART_CMD could not run: {e}"
                    ))
                }
            }
        }
        None => {
            let hint = suggest_restart();
            report.restart_command = Some(hint.clone());
            report.restart_ran = Some(false);
            if !o.quiet && !o.json {
                println!("  NOT restarting anything. To activate the new binaries:\n      {hint}");
            }
            Ok(())
        }
    }
}

/// A best-effort hint, from the unit files that are actually present. This is
/// only ever *printed*; it is never executed.
fn suggest_restart() -> String {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config"))
        .join("systemd")
        .join("user");
    for unit in [
        "gpn-fleet.service",
        "gpn-node.service",
        "gpn-ollama.service",
    ] {
        if dir.join(unit).is_file() {
            return format!("systemctl --user restart {unit}");
        }
    }
    "restart your node supervisor (e.g. `systemctl --user restart gpn-fleet`, \
     or re-run your fleet launcher)"
        .to_string()
}

// ─────────────────────────────────────────────────────────────────────────────
// Output
// ─────────────────────────────────────────────────────────────────────────────

fn emit_json(report: &Report) {
    match serde_json::to_string_pretty(report) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("gpn_update: could not serialise the report: {e}"),
    }
}

/// One line on stderr. With `--json` the message is also in the JSON object,
/// so a script parsing stdout needs no special case; with `--quiet` stderr is
/// still the correct place for a failure.
fn emit_error(o: &Options, report: &Report, msg: &str) {
    eprintln!("gpn_update: {msg}");
    if o.json {
        emit_json(report);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// main
// ─────────────────────────────────────────────────────────────────────────────

fn run(o: &Options) -> u8 {
    let (current, source) = installed_version(o);
    let mut report = Report {
        mode: match o.action {
            Action::Check => "check",
            Action::Apply => "apply",
            Action::Confirm => "confirm",
            Action::Rollback => "rollback",
            Action::RepairLauncher => "repair-launcher",
        },
        manifest_url: o.manifest_url.clone(),
        state_dir: o.state_dir.display().to_string(),
        target_dir: o.target_dir.display().to_string(),
        current,
        current_source: source,
        ..Default::default()
    };
    match o.action {
        Action::Check => run_check(o, &mut report),
        Action::Apply => run_apply(o, &mut report),
        Action::Confirm => run_confirm(o, &mut report),
        Action::Rollback => run_rollback(o, &mut report),
        Action::RepairLauncher => run_repair_launcher(o),
    }
}

fn main() -> ExitCode {
    let opts = match Options::from_env(std::env::args().skip(1)) {
        Ok(Some(o)) => o,
        Ok(None) => return ExitCode::from(EXIT_OK),
        Err(e) => {
            eprintln!("gpn_update: {e}");
            return ExitCode::from(EXIT_ERROR);
        }
    };
    ExitCode::from(run(&opts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_errors_and_transport_failures_are_retried_but_client_errors_are_not() {
        // The exact 2026-11-08 00:55 PT line: a GitHub-wide 504 on the asset.
        assert!(download_failure_is_transient(
            "download https://github.com/crewlyllc/gpn_coin/releases/download/\
             linux-latest/gpn-linux.tar.gz -> HTTP 504 Gateway Timeout"
        ));
        assert!(download_failure_is_transient("http://x/a -> HTTP 500"));
        assert!(download_failure_is_transient("http://x/a -> HTTP 599"));

        // A 4xx is the mirror answering that *this request* is wrong.
        for code in [400, 401, 403, 404, 408, 410, 429] {
            assert!(
                !download_failure_is_transient(&format!("http://x/a -> HTTP {code}")),
                "HTTP {code} must fail closed"
            );
        }

        // No status = the request never completed.
        for msg in [
            "download http://x/a: error sending request for url (http://x/a): connection reset by peer",
            "download body: error decoding response body: end of file before message length reached",
            "http client: operation timed out",
            "download http://x/a: connection closed before message completed",
        ] {
            assert!(download_failure_is_transient(msg), "{msg}");
        }

        // An error that is not a transport failure at all stays fatal.
        assert!(!download_failure_is_transient(
            "/tmp/x.tar.gz: No space left on device"
        ));
    }

    #[test]
    fn backoff_doubles_and_is_capped() {
        assert_eq!(retry_backoff_ms(1, 2_000), 2_000);
        assert_eq!(retry_backoff_ms(2, 2_000), 4_000);
        assert_eq!(retry_backoff_ms(3, 2_000), 8_000);
        // No overflow / unbounded sleep if the attempt count ever grows.
        assert_eq!(retry_backoff_ms(64, 2_000), 128_000);
    }

    #[test]
    fn a_permanent_failure_is_reported_once_without_the_attempt_count() {
        let mut calls = 0;
        let err = with_transient_retry::<u8>("download", || {
            calls += 1;
            Err("http://x/a -> HTTP 404 Not Found".to_string())
        })
        .unwrap_err();
        assert_eq!(calls, 1, "404 must not be retried");
        assert_eq!(err, "http://x/a -> HTTP 404 Not Found");
    }

    #[test]
    fn a_transient_failure_stops_after_the_attempt_budget() {
        let mut calls = 0;
        let err = with_transient_retry_base::<u8>("download", 0, || {
            calls += 1;
            Err("http://x/a -> HTTP 503".to_string())
        })
        .unwrap_err();
        assert_eq!(calls, DOWNLOAD_ATTEMPTS);
        assert!(err.contains("after 3 attempts"), "{err}");
    }
}
