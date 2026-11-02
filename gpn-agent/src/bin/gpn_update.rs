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
}

#[derive(Debug, Clone)]
struct Options {
    action: Action,
    json: bool,
    quiet: bool,
    state_dir: PathBuf,
    target_dir: PathBuf,
    manifest_url: String,
}

impl Options {
    fn from_env(args: impl Iterator<Item = String>) -> Result<Option<Options>, String> {
        let mut action: Option<Action> = None;
        let mut json = false;
        let mut quiet = false;
        let mut state_dir = None;
        let mut target_dir = None;
        let mut manifest_url = None;

        let mut it = args.peekable();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--check" => {
                    if action == Some(Action::Apply) {
                        return Err("--check and --apply are mutually exclusive".into());
                    }
                    action = Some(Action::Check);
                }
                "--apply" => {
                    if action == Some(Action::Check) {
                        return Err("--check and --apply are mutually exclusive".into());
                    }
                    action = Some(Action::Apply);
                }
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

        Ok(Some(Options {
            // No flag = --check: the safe, side-effect-free default.
            action: action.unwrap_or(Action::Check),
            json,
            quiet,
            state_dir: state_dir.unwrap_or_else(default_state_dir),
            target_dir: target_dir.unwrap_or_else(default_target_dir),
            manifest_url: manifest_url.unwrap_or_else(default_manifest_url),
        }))
    }
}

fn print_help() {
    println!(
        "\
gpn_update {COMPILED_VERSION} — verify and install signed GPN releases

USAGE:
  gpn_update [--check|--apply] [--json] [--quiet]
             [--state-dir DIR] [--target-dir DIR] [--url URL]

  --check        fetch + verify the manifest, print current vs latest.
                 Downloads nothing. Exit 3 when a newer release exists.
  --apply        download, verify, extract, flip `current`, copy the binaries
                 into the target dir, prune old versions.
  --json         one JSON object on stdout (machine-readable).
  --quiet        no stdout noise (the systemd timer uses this).
  --state-dir    where versions/ + current live (default {})
  --target-dir   where the binaries are installed (default {})
  --url          manifest URL (default {})

EXIT CODES: 0 up to date / applied, 1 error, 2 manifest unreachable,
            3 newer verified release available (or not applied by policy)

ENV: GPN_AUTO_UPDATE=0 disables --apply.
     GPN_UPDATE_RESTART_CMD is run after a successful swap; when unset the
     command to run is printed instead (nothing is restarted by default).",
        default_state_dir().display(),
        default_target_dir().display(),
        update::MANIFEST_URL,
    );
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
fn installed_version(target: &Path) -> (String, String) {
    for name in [update::NODE_BIN, update::RUST_NODE_BIN] {
        let p = target.join(name);
        if p.is_file() {
            if let Ok(Some(v)) = update::read_build_stamp(&p) {
                return (v, p.display().to_string());
            }
        }
    }
    (
        COMPILED_VERSION.to_string(),
        "compiled-in (no build stamp in the install dir)".to_string(),
    )
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
    let bundle = update::is_bundle(asset);
    report.layout = Some(update::asset_layout(asset));
    report.asset_url = Some(asset.url.clone());
    report.asset_size = Some(asset.size);
    report.asset_sha256 = Some(asset.sha256.clone());

    match install(o, report, &m, asset, bundle) {
        Ok(()) => {}
        Err(msg) => {
            report.error = Some(msg.clone());
            emit_error(o, report, &msg);
            return EXIT_ERROR;
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

    match restart(o, report) {
        Ok(()) => {
            if o.json {
                emit_json(report);
            }
            EXIT_OK
        }
        Err(msg) => {
            report.error = Some(msg.clone());
            emit_error(o, report, &msg);
            EXIT_ERROR
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
) -> Result<(), String> {
    let state = &o.state_dir;
    std::fs::create_dir_all(state).map_err(|e| format!("{}: {e}", state.display()))?;
    report.versions_dir = Some(update::versions_root(state).display().to_string());

    if !bundle {
        return install_single(o, report, m, asset);
    }

    let vdir = update::version_dir(state, &m.version);
    let already = update::bundle_installed(state, &m.version);
    if already {
        // A previous run downloaded and staged this exact version but died in
        // the copy step. Do not pull 300 MB again: re-verify what is on disk.
        update::verify_bundle_stamps(&vdir, &m.version).map_err(|e| e.to_string())?;
        update::verify_bundle_files(&vdir, &asset.files).map_err(|e| e.to_string())?;
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
        let staged = download_to_file(&asset.url, &tmp, Duration::from_secs(3600)).and_then(|_| {
            update::stage_bundle_file(state, &m.version, asset, &tmp).map_err(|e| e.to_string())
        });
        let _ = std::fs::remove_file(&tmp);
        staged?;
    }

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
                return Err(format!(
                    "could not flip `current` to {}: {}",
                    m.version, deferred.reason
                ))
            }
            Err(e) => return Err(format!("could not flip `current`: {e}")),
        }
    } else if update::current_version_dir(state).as_deref() != Some(vdir.as_path()) {
        update::activate_version(state, &m.version)
            .map_err(|e| format!("could not flip `current`: {e}"))?;
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
) -> Result<(), String> {
    let dir = o.state_dir.join("single").join(&m.version);
    let dest = dir.join(update::node_bin_name());
    if dest.is_file() {
        update::verify_file(&dest, asset).map_err(|e| e.to_string())?;
    } else {
        if !o.quiet && !o.json {
            println!(
                "gpn_update: downloading {} ({})",
                m.version,
                fmt_mb(asset.size)
            );
        }
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        // Buffered, but a single-layout asset is one binary (not a bundle).
        let body = fetch(&asset.url).map_err(|e| e.message().to_string())?;
        update::verify_bytes(&body, asset).map_err(|e| e.to_string())?;
        let tmp = dir.join(format!(
            ".{}.tmp-{}",
            update::node_bin_name(),
            std::process::id()
        ));
        std::fs::write(&tmp, &body).map_err(|e| format!("{}: {e}", tmp.display()))?;
        set_executable(&tmp)?;
        std::fs::rename(&tmp, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        update::verify_file(&dest, asset).map_err(|e| e.to_string())?;
    }
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
        if let Err(e) = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("chmod {}: {e}", tmp.display()));
        }
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
    let (current, source) = installed_version(&o.target_dir);
    let mut report = Report {
        mode: match o.action {
            Action::Check => "check",
            Action::Apply => "apply",
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
