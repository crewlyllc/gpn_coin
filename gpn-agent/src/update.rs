//! Signed release manifests, opt-in auto-update, and the minimum-version cutoff.
//!
//! ## The manifest
//!
//! `https://gpncoin.ai/releases.json` is the single source of truth for "what
//! is the current GPN node build". Its shape:
//!
//! ```json
//! {
//!   "version": "0.2.0",
//!   "min_version": "0.1.5",
//!   "published_at": "2026-10-21T17:00:00Z",
//!   "assets": [
//!     { "os": "linux", "arch": "x86_64", "layout": "bundle",
//!       "url": "https://github.com/…/gpn-linux.tar.gz",
//!       "sha256": "<64 hex>", "size": 331643208,
//!       "files": { "gpn_node": "<64 hex>" } }
//!   ],
//!   "signing_key": "<64 hex ed25519 public key>",
//!   "signature": "<128 hex ed25519 signature>"
//! }
//! ```
//!
//! ## Releases are bundles, not one binary
//!
//! A GPN release ships ~60 binaries (`gpn_node`, `gpn-node`, `gpn_scheduler`,
//! `gpn_llm_provider`, …). Replacing only `gpn_node` leaves the supervisor
//! running one version while every child it spawns is another — the child
//! binaries talk different RPC/p2p schemas and the node misbehaves in ways that
//! look like a chain problem. So an asset may carry `"layout": "bundle"`
//! (inferred from a `.tar.gz`/`.zip` URL when the field is absent) and the
//! updater then installs a **whole version directory**:
//!
//! ```text
//! ~/.gpn/node/update/
//!   versions/
//!     0.2.0/            <- complete, stamp-verified bundle (a rollback target)
//!     0.3.0/            <- the version being installed
//!   current -> versions/0.3.0      (symlink; a junction on Windows)
//!   current.previous -> versions/0.2.0   (one-step rollback pointer)
//!   pending-update.json
//! ```
//!
//! The switch is a single `rename` of the `current` link, so a node is never
//! half-upgraded: `current` points at a complete directory or it does not
//! change. `versions/<old>` is kept so a rollback is one link flip away.
//! `layout: "single"` (the old behaviour, `<exe>.new` + `<exe>.bak`) is still
//! supported for manifests that point at a lone binary.
//!
//! **The signature is over a canonical, line-oriented text blob — not over the
//! JSON** — so there is no JSON-canonicalisation ambiguity between the signer
//! (`scripts/gen-release-manifest.py`) and this verifier:
//!
//! ```text
//! gpn-release-v1
//! version=0.2.0
//! min_version=0.1.5
//! published_at=2026-10-21T17:00:00Z
//! signing_key=<64 hex>
//! asset=linux/x86_64 https://… <sha256> 12345678 layout=bundle
//! file=linux/x86_64 gpn_node <sha256>
//! asset=windows/x86_64 https://… <sha256> 12345678 layout=bundle
//! file=windows/x86_64 gpn_node.exe <sha256>
//! ```
//!
//! Assets are sorted by `(os, arch, url)` before signing/verifying, so the
//! asset order in the file is irrelevant; `file=` lines are sorted by path and
//! always follow their asset. Every line ends with `\n`, including the last.
//!
//! ` layout=` and `file=` are **additive**: an asset with no `layout` and no
//! `files` produces byte-identical canonical text to the pre-bundle format, so
//! manifests signed by an older signer keep verifying ([`canonical_text_legacy`]
//! is the exact old rendering, and [`verify_manifest`] accepts either). That
//! matters: a manifest format change that old nodes cannot verify would freeze
//! every deployed node on its last cached cutoff.
//!
//! **The 0.2.0 release is therefore signed in the legacy rendering**: the
//! deployed fleet is still on pre-bundle nodes, which compute the old text and
//! would reject anything else. Its assets still declare `layout: "bundle"` in
//! JSON (new nodes read it), and the *signed URL* ends in `.tar.gz`/`.zip`, which
//! is what [`asset_layout`] trusts first. From 0.3.0 on, once every node can
//! verify both renderings, publish with `--sign-layout` to bind `layout` and
//! per-file hashes.
//!
//! ## Key custody
//!
//! The ed25519 **private** key is NOT in this repo. It lives with GPN release
//! operations (documented in `docs/updates.md`); only the 32-byte public key is
//! compiled in ([`RELEASE_PUBKEY_HEX`]). Rotating the key therefore requires a
//! node release — which is the point: an attacker who can serve a forged
//! `releases.json` still cannot make a node accept a binary.
//!
//! ## Safety rules encoded here
//!
//! * An unparseable / unsigned / wrongly-keyed manifest is rejected **before**
//!   any bytes are downloaded. There is no "skip verification" path.
//! * The downloaded file is hashed twice: once in memory right after the
//!   download, and again from disk immediately before the swap.
//! * Staging writes to `<exe>.new.tmp-<pid>` and renames into place, so a
//!   half-written binary is never visible under a name we would exec.
//! * The previous binary is kept as `<exe>.bak` for one-step rollback.
//! * Auto-update is **ON by default** (opt-out): unless `GPN_AUTO_UPDATE=0`
//!   (or `auto_update = false`), releases are verified and installed. The *cutoff* check is
//!   separate and still runs — a manual-updater must be told it is too old.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where the signed manifest is served from.
pub const MANIFEST_URL: &str = "https://gpncoin.ai/releases.json";

/// The ed25519 release public key compiled into every node (32 bytes, hex).
///
/// Generated by `scripts/gpn-release-key.sh`; the matching private key is held
/// by GPN release operations only (`docs/updates.md` → "Key custody").
pub const RELEASE_PUBKEY_HEX: &str =
    "78015fab541c9a6230d3fb302a89f1a9950d1b181652cdf0a65901b6f0278c5e";

/// First line of the signed payload. Bumping it is a manifest format change and
/// requires a node release (by design). It is **not** bumped for the additive
/// `layout`/`file` fields: they render identically when absent, so one prefix
/// still covers both renderings.
pub const CANONICAL_PREFIX: &str = "gpn-release-v1";

/// Asset layout: one binary (the historical shape).
pub const LAYOUT_SINGLE: &str = "single";
/// Asset layout: an archive containing the whole release.
pub const LAYOUT_BUNDLE: &str = "bundle";
/// Directory under the update dir holding one complete tree per version.
pub const VERSIONS_DIR: &str = "versions";
/// Name of the symlink (junction on Windows) that selects the live version.
pub const CURRENT_LINK: &str = "current";
/// File recording what `current` pointed at before the last switch.
pub const PREVIOUS_POINTER: &str = "current.previous";
/// The stamp `gpn-agent/build.rs` injects into every GPN binary.
pub const BUILD_STAMP_PREFIX: &str = "gpn-build-v1 version=";
/// The supervisor binary, by name, inside a version directory.
pub const NODE_BIN: &str = "gpn_node";
/// The Rust node binary, by name, inside a version directory.
pub const RUST_NODE_BIN: &str = "gpn-node";

/// Suffix of a staged binary sitting next to the live one.
pub const STAGED_SUFFIX: &str = ".new";

/// Suffix of the one-step rollback copy of the previous binary.
pub const BACKUP_SUFFIX: &str = ".bak";

/// Name of the marker that says "a verified binary is staged".
pub const PENDING_FILE: &str = "pending-update.json";

// ─────────────────────────────────────────────────────────────────────────────
// Errors
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    /// The body was not the JSON object we expect.
    BadJson(String),
    /// No `signature` field (or an empty one).
    Unsigned,
    /// Hex decode / length failure on the signature or the key.
    BadHex(String),
    /// The manifest names a different signing key than the one we ship with.
    WrongKey { manifest: String, embedded: String },
    /// ed25519 verification failed: the manifest was edited, or signed by
    /// someone other than the holder of the release key.
    BadSignature,
    /// Downloaded bytes do not match the manifest's `sha256`/`size`.
    BadDigest { expected: String, actual: String },
    /// No asset for this OS/arch.
    NoAsset { os: String, arch: String },
    /// A bundled binary does not carry the build stamp the manifest claims.
    BadStamp {
        file: String,
        expected: String,
        actual: String,
    },
    /// The archive is not the shape a GPN release must have.
    BundleShape(String),
    /// Filesystem trouble.
    Io(String),
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UpdateError::BadJson(e) => write!(f, "manifest is not valid JSON: {e}"),
            UpdateError::Unsigned => write!(f, "manifest has no signature — refusing"),
            UpdateError::BadHex(e) => write!(f, "manifest signature/key is not hex: {e}"),
            UpdateError::WrongKey { manifest, embedded } => write!(
                f,
                "manifest is signed by key {manifest}, but this node only trusts {embedded}"
            ),
            UpdateError::BadSignature => {
                write!(
                    f,
                    "manifest signature does NOT verify against the GPN release key"
                )
            }
            UpdateError::BadDigest { expected, actual } => write!(
                f,
                "downloaded file hash {actual} does not match the manifest's {expected}"
            ),
            UpdateError::NoAsset { os, arch } => {
                write!(f, "manifest has no asset for {os}/{arch}")
            }
            UpdateError::BadStamp {
                file,
                expected,
                actual,
            } => write!(
                f,
                "{file} was built as {actual}, but the manifest publishes {expected} — \
                 refusing to install a mismatched release"
            ),
            UpdateError::BundleShape(e) => write!(f, "release bundle is unusable: {e}"),
            UpdateError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for UpdateError {}

impl From<std::io::Error> for UpdateError {
    fn from(e: std::io::Error) -> Self {
        UpdateError::Io(e.to_string())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Manifest
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseAsset {
    /// `linux` | `windows` | `macos` (lower-case).
    pub os: String,
    /// `x86_64` | `aarch64` (lower-case; `amd64`/`arm64` accepted on input).
    pub arch: String,
    pub url: String,
    /// Hex sha256 of the artifact.
    pub sha256: String,
    /// Byte length of the artifact (0 = "not declared", still hash-checked).
    #[serde(default)]
    pub size: u64,
    /// `bundle` | `single`. Empty means "infer from the URL" (see
    /// [`asset_layout`]): a `.tar.gz`/`.zip` is a bundle, anything else is a
    /// single binary. Signed when non-empty — see [`ReleaseManifest::canonical_text`].
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub layout: String,
    /// Per-file sha256 (bundle only), keyed by the path *inside* the archive.
    /// Signed. Optional: build-stamp verification already pins the version, and
    /// this pins individual files.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub files: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseManifest {
    pub version: String,
    pub min_version: String,
    #[serde(default)]
    pub published_at: String,
    #[serde(default)]
    pub assets: Vec<ReleaseAsset>,
    /// The ed25519 public key the `signature` was made with (hex). Must equal
    /// [`RELEASE_PUBKEY_HEX`] for the manifest to be accepted.
    #[serde(default)]
    pub signing_key: String,
    #[serde(default)]
    pub signature: String,
}

impl ReleaseManifest {
    /// The exact bytes that are signed / verified. See the module docs.
    ///
    /// This is the **current** (bundle-capable) rendering. For an asset with an
    /// empty `layout` and no `files` it is byte-identical to
    /// [`Self::canonical_text_legacy`], which is what every pre-bundle release
    /// signed.
    pub fn canonical_text(&self) -> Vec<u8> {
        self.canonical_text_impl(true)
    }

    /// The pre-bundle rendering: no ` layout=` suffix, no `file=` lines.
    ///
    /// Only used to keep a manifest signed by an older signer verifiable. It is
    /// never produced by a signer any more.
    pub fn canonical_text_legacy(&self) -> Vec<u8> {
        self.canonical_text_impl(false)
    }

    fn canonical_text_impl(&self, bundle_fields: bool) -> Vec<u8> {
        let mut s = String::new();
        s.push_str(CANONICAL_PREFIX);
        s.push('\n');
        s.push_str(&format!("version={}\n", self.version));
        s.push_str(&format!("min_version={}\n", self.min_version));
        s.push_str(&format!("published_at={}\n", self.published_at));
        // Hex is case-insensitive: normalise so a re-serialised manifest (a
        // proxy that upper-cases hex, a hand edit) still verifies.
        s.push_str(&format!(
            "signing_key={}\n",
            normalize_hex(&self.signing_key)
        ));
        let mut assets = self.assets.clone();
        assets.sort_by(|a, b| {
            (a.os.as_str(), a.arch.as_str(), a.url.as_str()).cmp(&(
                b.os.as_str(),
                b.arch.as_str(),
                b.url.as_str(),
            ))
        });
        for a in &assets {
            let mut line = format!(
                "asset={}/{} {} {} {}",
                a.os,
                a.arch,
                a.url,
                normalize_hex(&a.sha256),
                a.size
            );
            // Only emitted when the manifest says so: an old manifest must keep
            // producing the old bytes, or every deployed node stops verifying.
            let layout = a.layout.trim().to_ascii_lowercase();
            if bundle_fields && !layout.is_empty() {
                line.push_str(" layout=");
                line.push_str(&layout);
            }
            line.push('\n');
            s.push_str(&line);
            if bundle_fields {
                // BTreeMap: deterministic (sorted by path) iteration.
                for (name, sha) in &a.files {
                    s.push_str(&format!(
                        "file={}/{} {} {}\n",
                        a.os,
                        a.arch,
                        name,
                        normalize_hex(sha)
                    ));
                }
            }
        }
        s.into_bytes()
    }
}

/// Parse **and verify** a manifest body.
///
/// `pubkey_hex` is the key that must have signed it; production callers pass
/// [`RELEASE_PUBKEY_HEX`]. It is a parameter (not a constant) so tests can
/// stand up a throwaway key without weakening the production path.
pub fn verify_manifest(json: &[u8], pubkey_hex: &str) -> Result<ReleaseManifest, UpdateError> {
    let m: ReleaseManifest =
        serde_json::from_slice(json).map_err(|e| UpdateError::BadJson(e.to_string()))?;

    if m.signature.trim().is_empty() {
        return Err(UpdateError::Unsigned);
    }
    if m.signing_key.trim().is_empty() {
        return Err(UpdateError::Unsigned);
    }
    let embedded = normalize_hex(pubkey_hex);
    let declared = normalize_hex(&m.signing_key);
    if declared != embedded {
        return Err(UpdateError::WrongKey {
            manifest: declared,
            embedded,
        });
    }

    let key_bytes = hex::decode(&embedded).map_err(|e| UpdateError::BadHex(e.to_string()))?;
    let key_arr: [u8; 32] = key_bytes
        .as_slice()
        .try_into()
        .map_err(|_| UpdateError::BadHex("public key is not 32 bytes".into()))?;
    let sig_bytes = hex::decode(m.signature.trim())
        .map_err(|e| UpdateError::BadHex(format!("signature: {e}")))?;
    let sig_arr: [u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| UpdateError::BadHex("signature is not 64 bytes".into()))?;

    let vk = ed25519_dalek::VerifyingKey::from_bytes(&key_arr)
        .map_err(|e| UpdateError::BadSignature.with_detail(e.to_string()))?;
    let sig = ed25519_dalek::Signature::from_bytes(&sig_arr);

    use ed25519_dalek::Verifier;
    let text = m.canonical_text();
    if vk.verify(&text, &sig).is_ok() {
        return Ok(m);
    }
    // Accept a pre-bundle signature (no ` layout=` suffix / `file=` lines).
    // A manifest that *does* carry those fields cannot be downgraded this way:
    // the signature was made over the extended text, and the two differ.
    let legacy = m.canonical_text_legacy();
    if legacy != text && vk.verify(&legacy, &sig).is_ok() {
        return Ok(m);
    }
    Err(UpdateError::BadSignature)
}

impl UpdateError {
    fn with_detail(self, detail: String) -> Self {
        match self {
            UpdateError::BadSignature => UpdateError::BadSignature,
            UpdateError::BadHex(e) => UpdateError::BadHex(format!("{e}: {detail}")),
            other => other,
        }
    }
}

fn normalize_hex(s: &str) -> String {
    s.trim().trim_start_matches("0x").to_ascii_lowercase()
}

// ─────────────────────────────────────────────────────────────────────────────
// Platform selection
// ─────────────────────────────────────────────────────────────────────────────

/// `linux` / `windows` / `macos`.
pub fn platform_os() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// `x86_64` / `aarch64`.
pub fn platform_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    }
}

fn norm_arch(a: &str) -> &str {
    match a.trim().to_ascii_lowercase().as_str() {
        "amd64" | "x64" | "x86_64" => "x86_64",
        "arm64" | "aarch64" => "aarch64",
        other => Box::leak(other.to_string().into_boxed_str()),
    }
}

/// The asset for `(os, arch)`, if the manifest has one.
pub fn select_asset<'a>(
    m: &'a ReleaseManifest,
    os: &str,
    arch: &str,
) -> Result<&'a ReleaseAsset, UpdateError> {
    let want_arch = norm_arch(arch);
    m.assets
        .iter()
        .find(|a| a.os.trim().eq_ignore_ascii_case(os) && norm_arch(&a.arch) == want_arch)
        .ok_or_else(|| UpdateError::NoAsset {
            os: os.to_string(),
            arch: arch.to_string(),
        })
}

// ─────────────────────────────────────────────────────────────────────────────
// Layout: one binary, or a whole release bundle
// ─────────────────────────────────────────────────────────────────────────────

/// True when a URL names an archive rather than a lone executable.
pub fn looks_like_bundle_url(url: &str) -> bool {
    let u = url.trim().to_ascii_lowercase();
    let path = u.split(['?', '#']).next().unwrap_or(&u);
    [".tar.gz", ".tgz", ".tar", ".zip"]
        .iter()
        .any(|ext| path.ends_with(ext))
}

/// The asset's layout: what the **signed URL** says, then what the manifest
/// declares.
///
/// Order matters. The URL is inside the signature; `layout` is only signed when
/// the publisher opted into the extended rendering (see the module docs), and a
/// release that has to be verifiable by a pre-bundle node cannot use it. So:
///
/// * an archive URL (`.tar.gz`/`.tgz`/`.tar`/`.zip`) is **always** a bundle;
/// * otherwise a declared `layout` is honoured — it can only *enable* bundle
///   handling, never turn a signed archive URL back into a single binary;
/// * otherwise single.
///
/// That asymmetry is deliberate: a forged `layout` on a proxy-served manifest
/// cannot downgrade a 331 MB tarball into "the new `gpn_node`".
pub fn asset_layout(a: &ReleaseAsset) -> String {
    let declared = a.layout.trim().to_ascii_lowercase();
    let url_is_bundle = looks_like_bundle_url(&a.url);
    if url_is_bundle {
        return LAYOUT_BUNDLE.to_string();
    }
    if !declared.is_empty() {
        return declared;
    }
    LAYOUT_SINGLE.to_string()
}

/// Is this asset a multi-binary bundle?
pub fn is_bundle(a: &ReleaseAsset) -> bool {
    asset_layout(a) == LAYOUT_BUNDLE
}

// ─────────────────────────────────────────────────────────────────────────────
// Versioned install layout
// ─────────────────────────────────────────────────────────────────────────────

/// `<update dir>/versions` — one complete tree per installed version.
pub fn versions_root(state_dir: &Path) -> PathBuf {
    state_dir.join(VERSIONS_DIR)
}

/// `<update dir>/versions/<version>`.
pub fn version_dir(state_dir: &Path, version: &str) -> PathBuf {
    versions_root(state_dir).join(version.trim())
}

/// `<update dir>/current` — the symlink (junction on Windows) that selects the
/// live version directory.
pub fn current_link(state_dir: &Path) -> PathBuf {
    state_dir.join(CURRENT_LINK)
}

/// `<update dir>/current.previous` — where `current` pointed before the last
/// switch, i.e. the rollback target.
pub fn previous_pointer(state_dir: &Path) -> PathBuf {
    state_dir.join(PREVIOUS_POINTER)
}

/// File name of the supervisor for the platform we are *building* for.
pub fn node_bin_name() -> &'static str {
    if cfg!(windows) {
        "gpn_node.exe"
    } else {
        NODE_BIN
    }
}

fn rust_node_bin_name() -> &'static str {
    if cfg!(windows) {
        "gpn-node.exe"
    } else {
        RUST_NODE_BIN
    }
}

/// The supervisor executable inside `dir`.
pub fn node_bin(dir: &Path) -> PathBuf {
    dir.join(node_bin_name())
}

/// Does `versions/<version>` exist and look installed?
pub fn bundle_installed(state_dir: &Path, version: &str) -> bool {
    node_bin(&version_dir(state_dir, version)).is_file()
}

/// The version directory `current` points at, if any.
pub fn current_version_dir(state_dir: &Path) -> Option<PathBuf> {
    let link = current_link(state_dir);
    let target = std::fs::read_link(&link).ok()?;
    Some(if target.is_absolute() {
        target
    } else {
        state_dir.join(target)
    })
}

/// The version `current` names (its directory's file name), if any.
pub fn installed_version(state_dir: &Path) -> Option<String> {
    current_version_dir(state_dir)?
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

// ─────────────────────────────────────────────────────────────────────────────
// Versions
// ─────────────────────────────────────────────────────────────────────────────

/// Parse `M.m.p` (an optional leading `v` and a `-pre`/`+build` suffix are
/// ignored). Returns `None` for anything else — callers must fail *closed* on a
/// version they cannot read.
pub fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim();
    let v = v.strip_prefix('v').unwrap_or(v);
    let core = v.split(['-', '+']).next()?;
    let mut it = core.split('.');
    let major = it.next()?.trim().parse::<u64>().ok()?;
    let minor = it.next().unwrap_or("0").trim().parse::<u64>().ok()?;
    let patch = it.next().unwrap_or("0").trim().parse::<u64>().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// `Some(Ordering)` when both sides parse, `None` otherwise.
pub fn version_cmp(a: &str, b: &str) -> Option<Ordering> {
    Some(parse_version(a)?.cmp(&parse_version(b)?))
}

/// True only when `candidate` is **strictly** newer than `current` and both
/// parse. A garbage version never triggers an update.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    matches!(version_cmp(candidate, current), Some(Ordering::Greater))
}

/// The cutoff decision (pure).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    /// `current >= min_version` (or the manifest does not declare one).
    Supported,
    /// `current < min_version` — the node must not mine or announce.
    UpgradeRequired,
}

/// Decide whether this build is still allowed to participate.
///
/// Fails **open** on unparseable input: a malformed `min_version` must not be
/// able to brick a healthy node (the signature already proved GPN publishes
/// it), and an unparseable *our* version means the build is not a GPN release.
pub fn min_version_decision(current: &str, min_version: &str) -> Support {
    let min = min_version.trim();
    if min.is_empty() {
        return Support::Supported;
    }
    match version_cmp(current, min) {
        Some(Ordering::Less) => Support::UpgradeRequired,
        _ => Support::Supported,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Opt-in policy
// ─────────────────────────────────────────────────────────────────────────────

/// Is auto-update ON? **Default ON (opt-out)**: only an explicit no disables it.
pub fn auto_update_enabled(raw: Option<&str>) -> bool {
    match raw {
        None => true,
        Some(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off" | "n"
        ),
    }
}

/// `GPN_UPDATE_CHECK_HOURS` → a duration, clamped to a sane range so a typo
/// cannot turn into a request storm (min 1 h) or an never-checking node.
pub fn check_interval(hours: Option<u64>) -> Duration {
    Duration::from_secs(hours.unwrap_or(6).clamp(1, 168) * 3600)
}

// ─────────────────────────────────────────────────────────────────────────────
// Fleet-spread scheduling (never stampede the release host)
// ─────────────────────────────────────────────────────────────────────────────
//
// A fleet that installs the same release at the same time checks the same
// manifest at the same time, and — on release day — downloads 300 MB at the same
// time. Two independent guards, neither of which needs a server-side scheduler:
//
// * **Phase + jitter** spread the *checks* over the interval. A node's first
//   check happens at `start + phase`, where `phase` is derived from a stable
//   node identity, so a fleet of N nodes occupies N different seconds of the
//   interval instead of one. Each subsequent wait is `interval ± jitter`
//   (5–10 %), so the phases drift apart instead of re-converging.
// * **Install spread** delays the *download* of an update it just found by a
//   random 0–20 min (`GPN_UPDATE_SPREAD_SECS`), so even a fleet that checks in
//   the same minute does not start N simultaneous 300 MB transfers.
//
// The schedule is wall-clock friendly and restart friendly: a restart re-rolls
// the phase slightly (a bounded 12-bit bounce mixed into the identity hash), so
// a crash-looping node does not hammer the host at a fixed second, while a
// fleet that starts together still spreads over the whole interval.

/// Domain separator for the phase hash.
pub const PHASE_DOMAIN: &str = "gpn-update-phase-v1";
/// Domain separator for the per-check jitter hash.
pub const JITTER_DOMAIN: &str = "gpn-update-jitter-v1";
/// Domain separator for the install-spread hash.
pub const SPREAD_DOMAIN: &str = "gpn-update-spread-v1";

/// `GPN_UPDATE_SPREAD_SECS` default: up to 20 minutes before a found update is
/// downloaded, so N nodes never pull the same 300 MB archive at once.
pub const DEFAULT_SPREAD_SECS: u64 = 1200;
/// Upper clamp for the spread: a day is already absurd, and anything larger
/// would mean an update is found but not installed for a very long time.
pub const MAX_SPREAD_SECS: u64 = 86_400;
/// Bits of the restart hash mixed into the phase: a restart moves the phase by
/// at most `2^12` seconds' worth of bounce, i.e. "re-rolls lightly".
const RESTART_BOUNCE_BITS: u64 = 0x0fff;
/// Jitter is between 5 % and 10 % of the interval, per-mille.
pub const JITTER_MIN_PERMILLE: u64 = 50;
pub const JITTER_MAX_PERMILLE: u64 = 100;

/// A stable per-node seed.
///
/// `identity` must be **stable across restarts and unique per node** — the
/// supervisor passes the payout account (each node has its own), falling back to
/// the base-path string when no account is configured. Never pass secret
/// material: only a one-way hash of it is kept, but there is no reason to feed
/// a signer URI in.
pub fn schedule_seed(identity: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(PHASE_DOMAIN.as_bytes());
    h.update(b"\0identity\0");
    h.update(identity.trim().as_bytes());
    let out: [u8; 32] = h.finalize().into();
    out
}

fn mix(domain: &str, seed: &[u8], extra: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(domain.as_bytes());
    h.update(b"\0");
    h.update(seed);
    h.update(b"\0");
    h.update(extra);
    h.finalize().into()
}

fn u64_at(d: &[u8], at: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&d[at..at + 8]);
    u64::from_le_bytes(b)
}

/// The node's **phase**: where in `[0, interval)` its checks sit.
///
/// `start_unix` is the process start time. It only perturbs the identity-derived
/// phase through a 12-bit bounce, so:
///
/// * two nodes with different identities land at different points;
/// * the same node with the same start time computes the same phase (restarting
///   the *same second* is idempotent, which keeps the function pure and
///   testable);
/// * a genuine restart re-rolls the phase lightly rather than pinning a node to
///   one second of the day forever.
///
/// The result is always strictly less than `interval`.
pub fn check_phase(seed: &[u8], interval: Duration, start_unix: u64) -> Duration {
    let span = interval.as_secs();
    if span == 0 {
        return Duration::ZERO;
    }
    let base = mix(PHASE_DOMAIN, seed, b"phase");
    let bounce = mix(PHASE_DOMAIN, seed, &start_unix.to_le_bytes());
    let base_secs = u64_at(&base, 0);
    let bounce_secs = u64_at(&bounce, 0) & RESTART_BOUNCE_BITS;
    Duration::from_secs((base_secs ^ bounce_secs) % span)
}

/// Per-check jitter, in seconds, in `±[5 %, 10 %]` of `interval`.
///
/// `counter` is the number of checks this process has already done, so successive
/// checks of one node land at different offsets — nodes that happened to start
/// together drift apart instead of staying locked in step.
pub fn check_jitter(seed: &[u8], counter: u64, interval: Duration) -> i64 {
    let span = interval.as_secs();
    if span == 0 {
        return 0;
    }
    let d = mix(JITTER_DOMAIN, seed, &counter.to_le_bytes());
    let raw = u64_at(&d, 0);
    let range = JITTER_MAX_PERMILLE - JITTER_MIN_PERMILLE + 1;
    let permille = JITTER_MIN_PERMILLE + (raw % range);
    let magnitude = span.saturating_mul(permille) / 1000;
    // Sign from a different word of the same digest: independent of magnitude.
    if u64_at(&d, 8) & 1 == 0 {
        magnitude as i64
    } else {
        -(magnitude as i64)
    }
}

/// `interval + jitter`, never zero.
pub fn next_interval(interval: Duration, jitter_secs: i64) -> Duration {
    if jitter_secs >= 0 {
        interval + Duration::from_secs(jitter_secs as u64)
    } else {
        interval
            .saturating_sub(Duration::from_secs(jitter_secs.unsigned_abs()))
            .max(Duration::from_secs(1))
    }
}

/// `GPN_UPDATE_SPREAD_SECS` → the maximum install delay. `0` disables it.
pub fn spread_max(raw: Option<u64>) -> Duration {
    Duration::from_secs(raw.unwrap_or(DEFAULT_SPREAD_SECS).min(MAX_SPREAD_SECS))
}

/// The delay to wait between "an update is available" and starting the
/// download: uniform in `[0, spread]`, deterministic per `(node, version)`.
///
/// Per-version rather than per-check so a node that retries the same release
/// (a failed download) waits the same amount instead of re-rolling forever, and
/// two different releases do not reuse the same slot. `spread == 0` disables it
/// and returns zero.
pub fn spread_delay(spread: Duration, seed: &[u8], version: &str) -> Duration {
    let span = spread.as_secs();
    if span == 0 {
        return Duration::ZERO;
    }
    let d = mix(SPREAD_DOMAIN, seed, version.trim().as_bytes());
    // +1 so the whole closed range is reachable and every second is a slot.
    Duration::from_secs(u64_at(&d, 0) % (span + 1))
}

// ─────────────────────────────────────────────────────────────────────────────
// Hashing
// ─────────────────────────────────────────────────────────────────────────────

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

pub fn sha256_file(path: &Path) -> Result<String, UpdateError> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

/// Verify bytes against a manifest asset (`size` is checked only when the
/// manifest declares one).
pub fn verify_bytes(bytes: &[u8], asset: &ReleaseAsset) -> Result<(), UpdateError> {
    if asset.size > 0 && bytes.len() as u64 != asset.size {
        return Err(UpdateError::BadDigest {
            expected: format!("{} bytes", asset.size),
            actual: format!("{} bytes", bytes.len()),
        });
    }
    let actual = sha256_hex(bytes);
    if actual != normalize_hex(&asset.sha256) {
        return Err(UpdateError::BadDigest {
            expected: normalize_hex(&asset.sha256),
            actual,
        });
    }
    Ok(())
}

/// Verify a file on disk against a manifest asset.
pub fn verify_file(path: &Path, asset: &ReleaseAsset) -> Result<(), UpdateError> {
    if asset.size > 0 {
        let len = std::fs::metadata(path)?.len();
        if len != asset.size {
            return Err(UpdateError::BadDigest {
                expected: format!("{} bytes", asset.size),
                actual: format!("{len} bytes"),
            });
        }
    }
    let actual = sha256_file(path)?;
    if actual != normalize_hex(&asset.sha256) {
        return Err(UpdateError::BadDigest {
            expected: normalize_hex(&asset.sha256),
            actual,
        });
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Staging + atomic swap
// ─────────────────────────────────────────────────────────────────────────────

/// What is waiting to be swapped in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingUpdate {
    pub version: String,
    pub sha256: String,
    pub size: u64,
    /// Single layout: the staged binary (`<exe>.new`).
    /// Bundle layout: the complete, verified version directory.
    pub staged: String,
    /// The executable this update replaces (single) / the one to exec (bundle).
    pub exe: String,
    /// `bundle` | `single`; empty in a marker written by a pre-bundle node.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub layout: String,
    /// Bundle layout: `versions/<version>`. Empty for single layout.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version_dir: String,
    /// Bundle layout: per-file hashes re-checked immediately before the switch.
    /// Empty means "the stamp check is the only identity check" (a manifest
    /// that published no per-file hashes).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub files: BTreeMap<String, String>,
}

impl PendingUpdate {
    /// A marker from a pre-bundle node has no `layout`; treat it as single.
    pub fn layout_or_single(&self) -> &str {
        let l = self.layout.trim();
        if l.is_empty() {
            LAYOUT_SINGLE
        } else {
            l
        }
    }

    pub fn is_bundle(&self) -> bool {
        self.layout_or_single() == LAYOUT_BUNDLE
    }
}

/// Result of a successful swap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub version: String,
    /// Rollback target. Single layout: the previous binary (`<exe>.bak`).
    /// Bundle layout: the previous complete version directory (kept on disk).
    pub backup: PathBuf,
    /// What the supervisor must re-exec into. Single layout: the live exe.
    /// Bundle layout: `versions/<new>/gpn_node`.
    pub exec: PathBuf,
}

/// The swap was *not* performed because the OS refuses to replace a running
/// executable (Windows). The staged file stays validated and will be applied by
/// the next service start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwapDeferred {
    pub version: String,
    pub staged: PathBuf,
    pub reason: String,
}

pub fn staged_path(exe: &Path) -> PathBuf {
    PathBuf::from(format!("{}{}", exe.display(), STAGED_SUFFIX))
}

pub fn backup_path(exe: &Path) -> PathBuf {
    PathBuf::from(format!("{}{}", exe.display(), BACKUP_SUFFIX))
}

pub fn pending_path(state_dir: &Path) -> PathBuf {
    state_dir.join(PENDING_FILE)
}

/// Write `bytes` to `<exe>.new` — via a pid-unique temp file + rename, so a
/// crash mid-write can never leave a partial file under the staged name.
pub fn stage_bytes(
    state_dir: &Path,
    exe: &Path,
    version: &str,
    asset: &ReleaseAsset,
    bytes: &[u8],
) -> Result<PendingUpdate, UpdateError> {
    // Verify in memory FIRST: never write an unverified byte as a candidate.
    verify_bytes(bytes, asset)?;

    std::fs::create_dir_all(state_dir)?;
    let staged = staged_path(exe);
    let tmp = PathBuf::from(format!("{}.tmp-{}", staged.display(), std::process::id()));
    std::fs::write(&tmp, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, &staged)?;

    // Re-read from disk and verify again: the bytes that will be swapped in are
    // the ones on disk, not the ones we had in memory.
    verify_file(&staged, asset)?;

    let pending = PendingUpdate {
        version: version.to_string(),
        sha256: normalize_hex(&asset.sha256),
        size: asset.size,
        staged: staged.display().to_string(),
        exe: exe.display().to_string(),
        layout: LAYOUT_SINGLE.to_string(),
        version_dir: String::new(),
        files: BTreeMap::new(),
    };
    write_pending(state_dir, &pending)?;
    Ok(pending)
}

fn write_pending(state_dir: &Path, p: &PendingUpdate) -> Result<(), UpdateError> {
    std::fs::create_dir_all(state_dir)?;
    let body = serde_json::to_vec_pretty(p).map_err(|e| UpdateError::Io(e.to_string()))?;
    let target = pending_path(state_dir);
    let tmp = PathBuf::from(format!("{}.tmp-{}", target.display(), std::process::id()));
    std::fs::write(&tmp, &body)?;
    std::fs::rename(&tmp, &target)?;
    Ok(())
}

pub fn read_pending(state_dir: &Path) -> Option<PendingUpdate> {
    let body = std::fs::read(pending_path(state_dir)).ok()?;
    serde_json::from_slice(&body).ok()
}

/// Drop the marker and the staged file (used when staging turns out to be
/// unusable, e.g. a hash mismatch at swap time).
pub fn discard_pending(state_dir: &Path) {
    let _ = std::fs::remove_file(pending_path(state_dir));
    // The staged file name is unknown if the marker was unreadable; callers that
    // know the exe should prefer `discard_staged`.
}

pub fn discard_staged(state_dir: &Path, exe: &Path) {
    let _ = std::fs::remove_file(staged_path(exe));
    let _ = std::fs::remove_file(pending_path(state_dir));
}

/// Swap a staged binary in: verify it on disk **again**, keep the old binary as
/// `<exe>.bak`, then rename the staged file over the live path.
///
/// Returns `Ok(None)` when nothing is staged. The caller restarts (or is
/// restarted) afterwards — on Unix the rename succeeds while the old process is
/// still running; on Windows it does not, hence [`SwapDeferred`].
pub fn apply_pending(
    state_dir: &Path,
    exe: &Path,
) -> Result<Result<Option<Applied>, SwapDeferred>, UpdateError> {
    let Some(pending) = read_pending(state_dir) else {
        return Ok(Ok(None));
    };

    // A bundle install is not an exe swap: it flips `current` to a complete
    // version directory. Nothing about the running image is touched.
    if pending.is_bundle() {
        return apply_pending_bundle(state_dir, &pending);
    }

    let staged = PathBuf::from(&pending.staged);

    // Refuse a marker that points somewhere else than this binary: an operator
    // may have several nodes, and a stale marker must never clobber one of them.
    if !same_file_name(&pending.exe, exe) {
        discard_pending(state_dir);
        return Ok(Ok(None));
    }

    // Verify what is actually on disk, right before the swap.
    let actual = match sha256_file(&staged) {
        Ok(h) => h,
        Err(e) => {
            discard_pending(state_dir);
            return Err(e);
        }
    };
    let len = std::fs::metadata(&staged).map(|m| m.len()).unwrap_or(0);
    let bad = actual != pending.sha256 || (pending.size > 0 && len != pending.size);
    if bad {
        discard_staged(state_dir, exe);
        return Err(UpdateError::BadDigest {
            expected: pending.sha256,
            actual,
        });
    }

    // One-step rollback copy. Copy (not rename) so the live binary keeps
    // existing at its path until the very last instant.
    let backup = backup_path(exe);
    let backup_tmp = PathBuf::from(format!("{}.tmp-{}", backup.display(), std::process::id()));
    std::fs::copy(exe, &backup_tmp)?;
    let _ = std::fs::remove_file(&backup);
    std::fs::rename(&backup_tmp, &backup)?;

    match std::fs::rename(&staged, exe) {
        Ok(()) => {
            let _ = std::fs::remove_file(pending_path(state_dir));
            Ok(Ok(Some(Applied {
                version: pending.version,
                backup,
                exec: exe.to_path_buf(),
            })))
        }
        Err(e) => {
            // Windows: the running image is locked. Leave everything valid for
            // the next start rather than half-applying.
            let _ = std::fs::remove_file(&backup);
            Ok(Err(SwapDeferred {
                version: pending.version,
                staged,
                reason: e.to_string(),
            }))
        }
    }
}

fn same_file_name(a: &str, exe: &Path) -> bool {
    Path::new(a)
        .file_name()
        .map(|n| Some(n) == exe.file_name())
        .unwrap_or(false)
}

// ─────────────────────────────────────────────────────────────────────────────
// Bundles: build stamps, extraction, versioned staging, atomic switch
// ─────────────────────────────────────────────────────────────────────────────

/// Read the `gpn-build-v1 version=<M.m.p>` stamp out of a binary.
///
/// Returns `None` when the file carries no *real* stamp. A build embeds both the
/// real stamp and the `version=unknown` fallback `firstrun::build_id()` needs
/// when `build.rs` did not run, and the fallback's position is up to the linker,
/// so the first match cannot be trusted: every occurrence is parsed and the
/// first well-formed `M.m.p` wins.
pub fn read_build_stamp(path: &Path) -> Result<Option<String>, UpdateError> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let needle = BUILD_STAMP_PREFIX.as_bytes();
    let mut carry: Vec<u8> = Vec::new();
    let mut buf = [0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let mut window = std::mem::take(&mut carry);
        window.extend_from_slice(&buf[..n]);
        let mut start = 0usize;
        while let Some(idx) = find_subslice(&window[start..], needle) {
            let at = start + idx + needle.len();
            if let Some(v) = parse_stamp_version(&window[at..]) {
                return Ok(Some(v));
            }
            start = start + idx + 1;
        }
        // Keep enough tail that a stamp split across reads is still found.
        let keep = needle.len() + 64;
        let tail_from = window.len().saturating_sub(keep);
        carry = window[tail_from..].to_vec();
    }
    Ok(None)
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let first = needle[0];
    let mut i = 0usize;
    while i + needle.len() <= haystack.len() {
        match haystack[i..].iter().position(|b| *b == first) {
            Some(k) => {
                let at = i + k;
                if at + needle.len() > haystack.len() {
                    return None;
                }
                if &haystack[at..at + needle.len()] == needle {
                    return Some(at);
                }
                i = at + 1;
            }
            None => return None,
        }
    }
    None
}

/// Parse the version token right after the stamp prefix, if it is a real one.
fn parse_stamp_version(tail: &[u8]) -> Option<String> {
    let end = tail
        .iter()
        .position(|b| b.is_ascii_whitespace() || *b == 0)
        .unwrap_or(tail.len());
    let token = std::str::from_utf8(&tail[..end]).ok()?;
    if parse_version(token).is_none() || token.eq_ignore_ascii_case("unknown") {
        return None;
    }
    Some(token.trim().to_string())
}

/// Prove that the tree in `dir` really is version `version` before it can ever
/// become `current`.
///
/// * the **supervisor** (`gpn_node`) must carry a `gpn-build-v1` stamp and it
///   must be the manifest version — that is the binary this process execs;
/// * every *other* stamp found anywhere in the bundle must agree too (one
///   mismatched binary is the failure the bundle model exists to prevent);
/// * `gpn-node` must be present.
///
/// Only `gpn_node` is stamped in practice — `build.rs` lives in `gpn-agent`, and
/// `gpn-node` is a separate workspace crate with no build script — so a rule
/// that demanded a stamp in both rejected every real release.
pub fn verify_bundle_stamps(dir: &Path, version: &str) -> Result<(), UpdateError> {
    let want = version.trim();
    let supervisor = node_bin(dir);
    if !supervisor.is_file() {
        return Err(UpdateError::BundleShape(format!(
            "{} is missing from the bundle ({} expected)",
            node_bin_name(),
            want
        )));
    }
    if !dir.join(rust_node_bin_name()).is_file() {
        return Err(UpdateError::BundleShape(format!(
            "{} is missing from the bundle ({} expected)",
            rust_node_bin_name(),
            want
        )));
    }

    match read_build_stamp(&supervisor)? {
        Some(actual) if version_cmp(&actual, want) == Some(Ordering::Equal) => {}
        Some(actual) => {
            return Err(UpdateError::BadStamp {
                file: supervisor.display().to_string(),
                expected: want.to_string(),
                actual,
            })
        }
        None => {
            return Err(UpdateError::BadStamp {
                file: supervisor.display().to_string(),
                expected: want.to_string(),
                actual: "no gpn-build-v1 stamp".into(),
            })
        }
    }

    for entry in std::fs::read_dir(dir)? {
        let p = entry?.path();
        if !p.is_file() || p == supervisor {
            continue;
        }
        let Some(actual) = read_build_stamp(&p)? else {
            continue;
        };
        if version_cmp(&actual, want) != Some(Ordering::Equal) {
            return Err(UpdateError::BadStamp {
                file: p.display().to_string(),
                expected: want.to_string(),
                actual,
            });
        }
    }
    Ok(())
}

/// Verify every declared per-file hash inside `dir`.
pub fn verify_bundle_files(
    dir: &Path,
    files: &BTreeMap<String, String>,
) -> Result<(), UpdateError> {
    for (name, want) in files {
        let p = dir.join(name);
        if !p.is_file() {
            return Err(UpdateError::BundleShape(format!(
                "{name} is missing from the bundle"
            )));
        }
        let actual = sha256_file(&p)?;
        if actual != normalize_hex(want) {
            return Err(UpdateError::BadDigest {
                expected: normalize_hex(want),
                actual,
            });
        }
    }
    Ok(())
}

/// Extract a bundle archive into `dest` (which must already exist).
///
/// `.tar` / `.tar.gz` / `.tgz` use the pure-Rust `tar`+`flate2` crates we already
/// ship for `gpn_snapshot`. `.zip` has no pure-Rust extractor in the dependency
/// graph, so it uses the one Windows ships with (`Expand-Archive`); the Linux
/// updater never sees a `.zip`.
pub fn extract_bundle(bundle: &Path, dest: &Path) -> Result<(), UpdateError> {
    std::fs::create_dir_all(dest)?;
    let name = bundle
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let file = std::fs::File::open(bundle)?;
        let dec = flate2::read::GzDecoder::new(file);
        let mut ar = tar::Archive::new(dec);
        ar.unpack(dest)
            .map_err(|e| UpdateError::BundleShape(format!("tar.gz: {e}")))?;
    } else if name.ends_with(".tar") {
        let file = std::fs::File::open(bundle)?;
        let mut ar = tar::Archive::new(file);
        ar.unpack(dest)
            .map_err(|e| UpdateError::BundleShape(format!("tar: {e}")))?;
    } else if name.ends_with(".zip") {
        extract_zip(bundle, dest)?;
    } else {
        return Err(UpdateError::BundleShape(format!(
            "{name:?} is not a .tar.gz/.tar/.zip bundle"
        )));
    }
    flatten_single_root(dest)?;
    Ok(())
}

#[cfg(windows)]
fn extract_zip(bundle: &Path, dest: &Path) -> Result<(), UpdateError> {
    // `Expand-Archive` ships with PowerShell 5+, i.e. every supported Windows.
    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(format!(
            "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
            bundle.display(),
            dest.display()
        ))
        .status()?;
    if !status.success() {
        return Err(UpdateError::BundleShape(format!(
            "Expand-Archive failed ({status})"
        )));
    }
    Ok(())
}

#[cfg(not(windows))]
fn extract_zip(_bundle: &Path, _dest: &Path) -> Result<(), UpdateError> {
    Err(UpdateError::BundleShape(
        "a .zip bundle can only be installed on Windows (this host has no zip extractor)".into(),
    ))
}

/// A bundle packed as `tar -czf … <one-root-dir>` would otherwise install to
/// `versions/<v>/<root>/gpn_node`. Our releases are flat, but handle both.
fn flatten_single_root(dest: &Path) -> Result<(), UpdateError> {
    if node_bin(dest).is_file() {
        return Ok(());
    }
    let mut entries = std::fs::read_dir(dest)?
        .filter_map(|e| e.ok())
        .collect::<Vec<_>>();
    if entries.len() != 1 || !entries[0].path().is_dir() {
        return Ok(());
    }
    let root = entries.remove(0).path();
    for child in std::fs::read_dir(&root)?.filter_map(|e| e.ok()) {
        let to = dest.join(child.file_name());
        if to.exists() {
            return Err(UpdateError::BundleShape(format!(
                "{} would be overwritten while flattening {}",
                to.display(),
                root.display()
            )));
        }
        std::fs::rename(child.path(), &to)?;
    }
    let _ = std::fs::remove_dir(&root);
    Ok(())
}

/// Mark the executables in a freshly extracted version directory executable.
/// `tar` preserves modes, but a `.zip` (Windows-built, or unzipped on Unix)
/// does not carry them.
fn ensure_executable(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.filter_map(|e| e.ok()) {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with("gpn") {
                continue;
            }
            let p = e.path();
            if p.is_file() {
                let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755));
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
}

/// Stage a **bundle**: verify the archive hash, extract it into a versioned
/// staging directory, prove the build stamps match the manifest version, then
/// rename the whole directory into `versions/<version>`. Until that rename the
/// live tree is untouched; after it, `current` still points at the old version
/// until [`apply_pending`] flips it. There is no half-installed state.
pub fn stage_bundle_file(
    state_dir: &Path,
    version: &str,
    asset: &ReleaseAsset,
    bundle: &Path,
) -> Result<PendingUpdate, UpdateError> {
    // 1. The archive itself, before it is unpacked.
    verify_file(bundle, asset)?;

    let root = versions_root(state_dir);
    std::fs::create_dir_all(&root)?;
    let final_dir = version_dir(state_dir, version);
    let tmp = root.join(format!(".{}.tmp-{}", version.trim(), std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);

    let built = (|| -> Result<(), UpdateError> {
        extract_bundle(bundle, &tmp)?;
        ensure_executable(&tmp);
        // 2. The tree must BE the version the manifest sells us.
        verify_bundle_stamps(&tmp, version)?;
        verify_bundle_files(&tmp, &asset.files)?;
        Ok(())
    })();
    if let Err(e) = built {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }

    // 3. Publish the directory. If it already exists (a previous attempt, or a
    //    version installed by hand) reuse it only when it re-verifies.
    if final_dir.exists() {
        let ok = verify_bundle_stamps(&final_dir, version).is_ok()
            && verify_bundle_files(&final_dir, &asset.files).is_ok();
        if !ok {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(UpdateError::Io(format!(
                "{} already exists and does not verify — remove it and retry",
                final_dir.display()
            )));
        }
        let _ = std::fs::remove_dir_all(&tmp);
    } else if let Err(e) = std::fs::rename(&tmp, &final_dir) {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e.into());
    }

    // 4. A staging directory is useless on its own: record the switch to make.
    let pending = PendingUpdate {
        version: version.to_string(),
        sha256: normalize_hex(&asset.sha256),
        size: asset.size,
        staged: final_dir.display().to_string(),
        exe: node_bin(&final_dir).display().to_string(),
        layout: LAYOUT_BUNDLE.to_string(),
        version_dir: final_dir.display().to_string(),
        files: asset.files.clone(),
    };
    write_pending(state_dir, &pending)?;
    Ok(pending)
}

/// Point `current` at `target`.
///
/// The link is built under a temporary name and renamed over `current`, which is
/// one atomic directory-entry replacement: a reader either sees the old target
/// or the new one, never a missing or partial link.
#[cfg(unix)]
fn switch_current(state_dir: &Path, target: &Path) -> Result<(), UpdateError> {
    let link = current_link(state_dir);
    let tmp = state_dir.join(format!(".current.tmp-{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp)?;
    if let Err(e) = std::fs::rename(&tmp, &link) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

#[cfg(windows)]
fn switch_current(state_dir: &Path, target: &Path) -> Result<(), UpdateError> {
    // No symlinks by default (they need Developer Mode / SeCreateSymbolicLink):
    // use a directory junction, which `mklink /J` creates without privileges.
    let link = current_link(state_dir);
    let tmp = state_dir.join(format!(".current.tmp-{}", std::process::id()));
    if tmp.exists() {
        let _ = std::fs::remove_dir(&tmp);
    }
    let out = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&tmp)
        .arg(target)
        .output()?;
    if !out.status.success() {
        return Err(UpdateError::Io(format!(
            "mklink /J failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    // Removing a junction removes the link only, never the target's contents.
    if link.exists() {
        let _ = std::fs::remove_dir(&link);
    }
    if std::fs::rename(&tmp, &link).is_err() {
        let out = std::process::Command::new("cmd")
            .args(["/C", "move", "/Y"])
            .arg(&tmp)
            .arg(&link)
            .output()?;
        if !out.status.success() {
            return Err(UpdateError::Io(format!(
                "could not publish the current junction: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
    }
    Ok(())
}

fn write_previous_pointer(state_dir: &Path, previous: &Path) -> Result<(), UpdateError> {
    let p = previous_pointer(state_dir);
    let tmp = PathBuf::from(format!("{}.tmp-{}", p.display(), std::process::id()));
    std::fs::write(&tmp, format!("{}\n", previous.display()))?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

fn apply_pending_bundle(
    state_dir: &Path,
    pending: &PendingUpdate,
) -> Result<Result<Option<Applied>, SwapDeferred>, UpdateError> {
    let vdir = PathBuf::from(&pending.version_dir);
    if pending.version_dir.trim().is_empty() || !node_bin(&vdir).is_file() {
        discard_pending(state_dir);
        return Err(UpdateError::BundleShape(format!(
            "{} is not a complete version directory",
            pending.version_dir
        )));
    }

    // Re-verify what is on disk right before the switch — the directory may have
    // been sitting there since a previous boot.
    verify_bundle_stamps(&vdir, &pending.version)?;
    if !pending.files.is_empty() {
        verify_bundle_files(&vdir, &pending.files)?;
    }

    let previous = current_version_dir(state_dir);
    if let Err(e) = switch_current(state_dir, &vdir) {
        // `current` still points at the old complete tree: nothing half-swapped.
        return Ok(Err(SwapDeferred {
            version: pending.version.clone(),
            staged: vdir,
            reason: e.to_string(),
        }));
    }
    if let Some(prev) = &previous {
        let _ = write_previous_pointer(state_dir, prev);
    }
    let _ = std::fs::remove_file(pending_path(state_dir));
    prune_versions(state_dir, &vdir, previous.as_deref());

    Ok(Ok(Some(Applied {
        version: pending.version.clone(),
        backup: previous.unwrap_or_default(),
        exec: node_bin(&vdir),
    })))
}

/// Keep `current` and one rollback target; drop older version directories so a
/// node does not accumulate a gigabyte per release.
fn prune_versions(state_dir: &Path, current: &Path, previous: Option<&Path>) {
    let Ok(rd) = std::fs::read_dir(versions_root(state_dir)) else {
        return;
    };
    for e in rd.filter_map(|e| e.ok()) {
        let p = e.path();
        if !p.is_dir() || p == current || previous.map(|x| x == p).unwrap_or(false) {
            continue;
        }
        let _ = std::fs::remove_dir_all(&p);
    }
}

/// Flip `current` back to the version it pointed at before the last switch.
///
/// One step, and it only ever selects a directory that already passed stamp
/// verification. Returns the version directory now live, or `None` when there is
/// nothing to roll back to.
pub fn rollback_bundle(state_dir: &Path) -> Result<Option<PathBuf>, UpdateError> {
    let Ok(body) = std::fs::read(previous_pointer(state_dir)) else {
        return Ok(None);
    };
    let prev = PathBuf::from(String::from_utf8_lossy(&body).trim().to_string());
    if !node_bin(&prev).is_file() {
        return Err(UpdateError::BundleShape(format!(
            "rollback target {} is gone",
            prev.display()
        )));
    }
    let now = current_version_dir(state_dir);
    switch_current(state_dir, &prev)?;
    if let Some(now) = now {
        let _ = write_previous_pointer(state_dir, &now);
    }
    Ok(Some(prev))
}

// ─────────────────────────────────────────────────────────────────────────────
// Cached state (so the cutoff survives an offline start)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateState {
    /// Unix seconds of the last successful manifest fetch.
    #[serde(default)]
    pub last_check: u64,
    pub latest_version: String,
    pub min_version: String,
    #[serde(default)]
    pub staged_version: Option<String>,
}

const STATE_FILE: &str = "update-state.json";

pub fn state_path(state_dir: &Path) -> PathBuf {
    state_dir.join(STATE_FILE)
}

pub fn load_state(state_dir: &Path) -> Option<UpdateState> {
    let body = std::fs::read(state_path(state_dir)).ok()?;
    serde_json::from_slice(&body).ok()
}

pub fn save_state(state_dir: &Path, st: &UpdateState) -> Result<(), UpdateError> {
    std::fs::create_dir_all(state_dir)?;
    let body = serde_json::to_vec_pretty(st).map_err(|e| UpdateError::Io(e.to_string()))?;
    let target = state_path(state_dir);
    let tmp = PathBuf::from(format!("{}.tmp-{}", target.display(), std::process::id()));
    std::fs::write(&tmp, &body)?;
    std::fs::rename(&tmp, &target)?;
    Ok(())
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Set a boolean key in a TOML document, preserving everything else.
///
/// Used by `--setup` to persist the auto-update answer without rewriting an
/// operator's config file. Compares whole keys (`auto_update` never matches
/// `update_auto_update`) and appends the key when absent.
pub fn set_toml_bool(text: &str, key: &str, value: bool) -> String {
    let want = format!("{key} = {value}");
    let mut out = String::with_capacity(text.len() + want.len() + 2);
    let mut found = false;
    for line in text.lines() {
        let t = line.trim();
        let is_key = t
            .split_once('=')
            .map(|(k, _)| k.trim() == key)
            .unwrap_or(false);
        if !is_key {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if !found {
            out.push_str(&want);
            out.push('\n');
            found = true;
        }
        // A duplicated key: drop the extra line (TOML would reject it).
    }
    if !found {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&want);
        out.push('\n');
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer;

    /// Deterministic throwaway key for tests only. NEVER a real release key.
    fn test_key() -> (ed25519_dalek::SigningKey, String) {
        let seed = [7u8; 32];
        let sk = ed25519_dalek::SigningKey::from_bytes(&seed);
        let pk = hex::encode(sk.verifying_key().to_bytes());
        (sk, pk)
    }

    fn sample_asset() -> ReleaseAsset {
        ReleaseAsset {
            os: "linux".into(),
            arch: "x86_64".into(),
            url: "https://gpncoin.ai/dl/gpn_node-0.2.0-linux-x86_64".into(),
            sha256: sha256_hex(b"new-binary-bytes"),
            size: b"new-binary-bytes".len() as u64,
            layout: String::new(),
            files: BTreeMap::new(),
        }
    }

    fn bundle_asset(url: &str, bytes: &[u8]) -> ReleaseAsset {
        ReleaseAsset {
            os: "linux".into(),
            arch: "x86_64".into(),
            url: url.into(),
            sha256: sha256_hex(bytes),
            size: bytes.len() as u64,
            layout: LAYOUT_BUNDLE.into(),
            files: BTreeMap::new(),
        }
    }

    fn sample_manifest() -> ReleaseManifest {
        ReleaseManifest {
            version: "0.2.0".into(),
            min_version: "0.1.5".into(),
            published_at: "2026-10-21T17:00:00Z".into(),
            assets: vec![sample_asset()],
            signing_key: String::new(),
            signature: String::new(),
        }
    }

    /// Sign a manifest with `sk` and return the JSON body a node would fetch.
    fn signed_json(m: &ReleaseManifest, sk: &ed25519_dalek::SigningKey) -> Vec<u8> {
        let mut m = m.clone();
        m.signing_key = hex::encode(sk.verifying_key().to_bytes());
        let sig = sk.sign(&m.canonical_text());
        m.signature = hex::encode(sig.to_bytes());
        serde_json::to_vec_pretty(&m).unwrap()
    }

    fn td(tag: &str) -> PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p =
            std::env::temp_dir().join(format!("gpn-update-test-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    // ── manifest verification ────────────────────────────────────────────────

    #[test]
    fn good_manifest_verifies() {
        let (sk, pk) = test_key();
        let body = signed_json(&sample_manifest(), &sk);
        let m = verify_manifest(&body, &pk).expect("a correctly signed manifest must verify");
        assert_eq!(m.version, "0.2.0");
        assert_eq!(m.min_version, "0.1.5");
    }

    #[test]
    fn uppercase_key_and_signature_hex_still_verify() {
        let (sk, pk) = test_key();
        let body = signed_json(&sample_manifest(), &sk);
        let mut m: ReleaseManifest = serde_json::from_slice(&body).unwrap();
        m.signing_key = m.signing_key.to_uppercase();
        m.signature = m.signature.to_uppercase();
        let body = serde_json::to_vec(&m).unwrap();
        assert!(verify_manifest(&body, &pk.to_uppercase()).is_ok());
    }

    #[test]
    fn tampered_version_fails_signature() {
        let (sk, pk) = test_key();
        let body = signed_json(&sample_manifest(), &sk);
        let mut v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        v["version"] = serde_json::json!("9.9.9");
        let tampered = serde_json::to_vec(&v).unwrap();
        assert_eq!(
            verify_manifest(&tampered, &pk),
            Err(UpdateError::BadSignature)
        );
    }

    #[test]
    fn tampered_min_version_fails_signature() {
        let (sk, pk) = test_key();
        let body = signed_json(&sample_manifest(), &sk);
        let mut v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        v["min_version"] = serde_json::json!("99.0.0");
        let tampered = serde_json::to_vec(&v).unwrap();
        assert_eq!(
            verify_manifest(&tampered, &pk),
            Err(UpdateError::BadSignature),
            "the cutoff must be as tamper-proof as the artifact"
        );
    }

    #[test]
    fn tampered_sha256_fails_signature() {
        let (sk, pk) = test_key();
        let body = signed_json(&sample_manifest(), &sk);
        let mut v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        v["assets"][0]["sha256"] = serde_json::json!(sha256_hex(b"evil-binary"));
        let tampered = serde_json::to_vec(&v).unwrap();
        assert_eq!(
            verify_manifest(&tampered, &pk),
            Err(UpdateError::BadSignature)
        );
    }

    #[test]
    fn added_asset_fails_signature() {
        let (sk, pk) = test_key();
        let body = signed_json(&sample_manifest(), &sk);
        let mut v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        v["assets"].as_array_mut().unwrap().push(serde_json::json!({
            "os": "linux", "arch": "aarch64",
            "url": "https://evil.example/x",
            "sha256": sha256_hex(b"evil"), "size": 4
        }));
        let tampered = serde_json::to_vec(&v).unwrap();
        assert_eq!(
            verify_manifest(&tampered, &pk),
            Err(UpdateError::BadSignature),
            "an asset cannot be smuggled in — the canonical text binds every asset"
        );
    }

    #[test]
    fn signature_from_a_different_key_is_rejected() {
        let (sk, _pk) = test_key();
        let other = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        let trusted = hex::encode(sk.verifying_key().to_bytes());
        let body = signed_json(&sample_manifest(), &other);
        match verify_manifest(&body, &trusted) {
            Err(UpdateError::WrongKey { .. }) => {}
            other => panic!("expected WrongKey, got {other:?}"),
        }
    }

    #[test]
    fn unsigned_manifest_is_rejected() {
        let (sk, pk) = test_key();
        let mut m = sample_manifest();
        m.signing_key = hex::encode(sk.verifying_key().to_bytes());
        let body = serde_json::to_vec(&m).unwrap();
        assert_eq!(verify_manifest(&body, &pk), Err(UpdateError::Unsigned));
    }

    #[test]
    fn empty_signature_is_rejected() {
        let (sk, pk) = test_key();
        let body = signed_json(&sample_manifest(), &sk);
        let mut v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        v["signature"] = serde_json::json!("");
        assert_eq!(
            verify_manifest(&serde_json::to_vec(&v).unwrap(), &pk),
            Err(UpdateError::Unsigned)
        );
    }

    #[test]
    fn malformed_json_is_rejected() {
        let (_sk, pk) = test_key();
        assert!(matches!(
            verify_manifest(b"{not json", &pk),
            Err(UpdateError::BadJson(_))
        ));
    }

    #[test]
    fn canonical_text_sorts_assets_and_ends_with_newline() {
        let mut m = sample_manifest();
        m.assets = vec![
            ReleaseAsset {
                os: "windows".into(),
                arch: "x86_64".into(),
                url: "https://gpncoin.ai/dl/win".into(),
                sha256: "aa".into(),
                size: 1,
                layout: String::new(),
                files: BTreeMap::new(),
            },
            sample_asset(),
        ];
        let text = String::from_utf8(m.canonical_text()).unwrap();
        assert!(text.starts_with("gpn-release-v1\n"));
        assert!(text.ends_with('\n'));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 7, "prefix + 4 header fields + 2 assets");
        assert!(lines[5].starts_with("asset=linux/x86_64"));
        assert!(lines[6].starts_with("asset=windows/x86_64"));

        // Order in the file must not matter.
        let mut flipped = m.clone();
        flipped.assets.reverse();
        assert_eq!(m.canonical_text(), flipped.canonical_text());
    }

    // ── hashing ─────────────────────────────────────────────────────────────

    #[test]
    fn wrong_sha256_is_rejected() {
        let bytes = b"new-binary-bytes";
        let mut a = sample_asset();
        a.sha256 = sha256_hex(b"something-else");
        assert!(matches!(
            verify_bytes(bytes, &a),
            Err(UpdateError::BadDigest { .. })
        ));
    }

    #[test]
    fn wrong_size_is_rejected() {
        let bytes = b"new-binary-bytes";
        let mut a = sample_asset();
        a.size = 1;
        assert!(matches!(
            verify_bytes(bytes, &a),
            Err(UpdateError::BadDigest { .. })
        ));
    }

    #[test]
    fn declared_zero_size_skips_the_length_check() {
        let bytes = b"new-binary-bytes";
        let mut a = sample_asset();
        a.size = 0;
        assert!(verify_bytes(bytes, &a).is_ok());
    }

    // ── versions ────────────────────────────────────────────────────────────

    #[test]
    fn version_parsing_and_ordering() {
        assert_eq!(parse_version("0.2.0"), Some((0, 2, 0)));
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2.3-rc1"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("junk"), None);
        assert_eq!(parse_version(""), None);

        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(!is_newer("0.2.0", "0.2.0"), "equal is not newer");
        assert!(!is_newer("0.1.9", "0.2.0"));
        assert!(is_newer("0.10.0", "0.9.9"), "numeric, not lexical");
        assert!(!is_newer("nonsense", "0.1.0"), "garbage never updates");
    }

    /// `scripts/bump-version.py` is the only thing that writes the package
    /// version; if it ever produced something `parse_version` cannot read, the
    /// updater would silently never fire (is_newer fails closed).
    #[test]
    fn package_version_is_plain_major_minor_patch() {
        let v = env!("CARGO_PKG_VERSION");
        let parsed = parse_version(v).unwrap_or_else(|| panic!("crate version {v:?} is not M.m.p"));
        assert_eq!(
            format!("{}.{}.{}", parsed.0, parsed.1, parsed.2),
            v,
            "crate version {v:?} must have no pre-release/build suffix or leading zeroes"
        );
        assert!(is_newer(v, "0.0.0"), "{v:?} must be strictly positive");
    }

    #[test]
    fn min_version_cutoff_decisions() {
        assert_eq!(
            min_version_decision("0.1.0", "0.1.5"),
            Support::UpgradeRequired
        );
        assert_eq!(min_version_decision("0.1.5", "0.1.5"), Support::Supported);
        assert_eq!(min_version_decision("0.2.0", "0.1.5"), Support::Supported);
        // Fail open on garbage so a typo cannot brick a live node.
        assert_eq!(min_version_decision("0.2.0", "wat"), Support::Supported);
        assert_eq!(min_version_decision("0.2.0", ""), Support::Supported);
    }

    // ── opt-in ──────────────────────────────────────────────────────────────

    #[test]
    fn auto_update_is_opt_out() {
        assert!(auto_update_enabled(None), "default must be ON");
        assert!(auto_update_enabled(Some("")));
        assert!(!auto_update_enabled(Some("0")));
        assert!(!auto_update_enabled(Some("false")));
        assert!(!auto_update_enabled(Some("no")));
        assert!(!auto_update_enabled(Some("n")));
        assert!(auto_update_enabled(Some("1")));
        assert!(auto_update_enabled(Some("true")));
        assert!(auto_update_enabled(Some(" YES ")));
        assert!(auto_update_enabled(Some("on")));
    }

    #[test]
    fn check_interval_is_clamped() {
        assert_eq!(check_interval(None), Duration::from_secs(6 * 3600));
        assert_eq!(check_interval(Some(0)), Duration::from_secs(3600));
        assert_eq!(check_interval(Some(12)), Duration::from_secs(12 * 3600));
        assert_eq!(check_interval(Some(99999)), Duration::from_secs(168 * 3600));
    }

    // ── fleet spread: phase, jitter, install delay ───────────────────────────

    #[test]
    fn phase_is_inside_the_interval() {
        let interval = check_interval(None); // 6 h
        for i in 0..500 {
            let seed = schedule_seed(&format!("account:node-{i}"));
            let p = check_phase(&seed, interval, 1_700_000_000 + i);
            assert!(p < interval, "phase {p:?} must be < {interval:?}");
        }
        // A zero interval can never produce a nonzero phase.
        assert_eq!(check_phase(b"seed", Duration::ZERO, 1), Duration::ZERO);
    }

    #[test]
    fn phase_is_deterministic_per_identity() {
        let interval = check_interval(None);
        let a = schedule_seed("account:5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY");
        let b = schedule_seed("account:5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty");
        let start = 1_766_000_000u64;
        assert_eq!(
            check_phase(&a, interval, start),
            check_phase(&a, interval, start),
            "same node, same start → same phase (the function is pure)"
        );
        assert_ne!(
            check_phase(&a, interval, start),
            check_phase(&b, interval, start),
            "different nodes must not share a phase"
        );
        // The seed itself is stable and identity-sensitive.
        assert_eq!(schedule_seed("account:x"), schedule_seed(" account:x "));
        assert_ne!(schedule_seed("account:x"), schedule_seed("account:y"));
    }

    #[test]
    fn a_restart_moves_the_phase_only_lightly() {
        // The bounce is a 12-bit mix, so a restart shifts the phase by at most
        // ~68 min on a 6 h interval — never to the far side of the day.
        let interval = check_interval(None);
        let seed = schedule_seed("account:restarty");
        let base = check_phase(&seed, interval, 1_766_000_000).as_secs();
        for k in 0..50u64 {
            let p = check_phase(&seed, interval, 1_766_000_000 + k * 3600).as_secs();
            let drift = (p as i64 - base as i64).unsigned_abs();
            let drift = drift.min(interval.as_secs() - drift);
            assert!(
                drift <= RESTART_BOUNCE_BITS,
                "a restart must re-roll the phase lightly, drifted {drift}s"
            );
        }
    }

    #[test]
    fn jitter_is_within_five_to_ten_percent() {
        let interval = check_interval(None);
        let seed = schedule_seed("account:jittery");
        let lo = interval.as_secs() * JITTER_MIN_PERMILLE / 1000;
        let hi = interval.as_secs() * JITTER_MAX_PERMILLE / 1000;
        let mut up = 0;
        let mut down = 0;
        for counter in 0..200 {
            let j = check_jitter(&seed, counter, interval);
            assert!(
                (lo..=hi).contains(&j.unsigned_abs()),
                "jitter {j}s outside ±[{lo},{hi}]s at counter {counter}"
            );
            if j > 0 {
                up += 1
            } else {
                down += 1
            }
        }
        assert!(up > 40 && down > 40, "jitter must spread both ways ({up}/{down})");
        // Deterministic per (node, counter), and counters differ.
        assert_eq!(check_jitter(&seed, 7, interval), check_jitter(&seed, 7, interval));
        assert_ne!(check_jitter(&seed, 7, interval), check_jitter(&seed, 8, interval));
        assert_eq!(check_jitter(&seed, 0, Duration::ZERO), 0);
    }

    #[test]
    fn next_interval_applies_the_jitter_and_never_reaches_zero() {
        let interval = Duration::from_secs(21_600);
        assert_eq!(
            next_interval(interval, 600),
            Duration::from_secs(22_200)
        );
        assert_eq!(
            next_interval(interval, -600),
            Duration::from_secs(21_000)
        );
        assert_eq!(next_interval(interval, 0), interval);
        assert_eq!(
            next_interval(Duration::from_secs(10), -9_999),
            Duration::from_secs(1),
            "a silly negative jitter must not produce a spin loop"
        );
    }

    /// The point of the whole exercise: 100 nodes started at the same instant
    /// must not all check at the same instant. Every node lands somewhere in the
    /// interval, and no coarse bucket holds a stampede.
    #[test]
    fn a_hundred_nodes_spread_over_the_whole_interval() {
        let interval = check_interval(None);
        let start = 1_766_000_000u64;
        let bucket = 60u64; // coarse 1-minute buckets
        let buckets_total = interval.as_secs() / bucket;
        let mut seen_second = std::collections::HashSet::new();
        let mut hist = std::collections::BTreeMap::<u64, u32>::new();
        let mut deciles = [0u32; 10];

        for i in 0..100 {
            let seed = schedule_seed(&format!("account:fleet-node-{i}"));
            let p = check_phase(&seed, interval, start).as_secs();
            assert!(p < interval.as_secs());
            assert!(seen_second.insert(p), "two nodes check in the same second");
            *hist.entry(p / bucket).or_default() += 1;
            deciles[((p * 10) / interval.as_secs()) as usize] += 1;
        }

        // With 100 nodes over 360 one-minute buckets a uniform hash still
        // collides sometimes (birthday: E[distinct] ≈ 87.5). What must not
        // happen is the fleet clustering: >80 % of nodes in their own bucket and
        // never more than a pair anywhere.
        let max_in_bucket = hist.values().copied().max().unwrap_or(0);
        eprintln!(
            "fleet spread: {} distinct minute buckets, max {}, deciles {deciles:?}",
            hist.len(),
            max_in_bucket
        );
        assert!(
            hist.len() >= 80,
            "nodes must occupy their own minute buckets, got {} of 100",
            hist.len()
        );
        assert!(
            max_in_bucket <= 5,
            "a minute bucket must never hold a crowd (<=5 % of the fleet): {hist:?}"
        );
        assert!(
            hist.len() < buckets_total as usize,
            "the fleet must not fill the interval (that would mean a degenerate hash)"
        );
        assert!(
            deciles.iter().all(|n| *n > 0),
            "coverage must reach every tenth of the interval, got {deciles:?}"
        );
        // A perfectly uniform set would put 10 nodes in each decile; allow the
        // hash to be lumpy, but not clustered into one part of the day.
        assert!(
            deciles.iter().all(|n| *n >= 3),
            "no decile may be nearly empty: {deciles:?}"
        );
    }

    /// jitter keeps an already-spread fleet from re-converging over time.
    #[test]
    fn successive_checks_drift_apart() {
        let interval = check_interval(None);
        let seeds: Vec<_> = (0..10)
            .map(|i| schedule_seed(&format!("account:drift-{i}")))
            .collect();
        // All ten start at the same phase (same start time) is not what happens
        // in the fleet, but the jitter must separate them regardless.
        let jumps: Vec<i64> = seeds
            .iter()
            .map(|s| check_jitter(s, 1, interval))
            .collect();
        let distinct: std::collections::HashSet<_> = jumps.iter().collect();
        assert_eq!(distinct.len(), jumps.len(), "jitter must differ per node");
    }

    #[test]
    fn spread_delay_is_bounded_and_disabled_at_zero() {
        let spread = spread_max(None);
        assert_eq!(spread, Duration::from_secs(1200));
        assert_eq!(spread_max(Some(0)), Duration::ZERO);
        assert_eq!(spread_max(Some(60)), Duration::from_secs(60));
        assert_eq!(
            spread_max(Some(999_999_999)),
            Duration::from_secs(MAX_SPREAD_SECS),
            "an absurd value is clamped, not honoured"
        );

        let seed = schedule_seed("account:spready");
        let mut seen = std::collections::HashSet::new();
        for i in 0..500 {
            let d = spread_delay(spread, &seed, &format!("0.3.{i}"));
            assert!(d <= spread, "delay {d:?} exceeds the spread {spread:?}");
            seen.insert(d.as_secs());
        }
        assert!(seen.len() > 400, "the delay must be spread, not constant");
        assert!(*seen.iter().min().unwrap() < 60, "some node goes nearly at once");
        assert!(*seen.iter().max().unwrap() > 1100, "some node waits most of the window");

        // Per (node, version): stable on retry, different for another release.
        assert_eq!(
            spread_delay(spread, &seed, "0.3.0"),
            spread_delay(spread, &seed, "0.3.0")
        );
        assert_ne!(
            spread_delay(spread, &seed, "0.3.0"),
            spread_delay(spread, &seed, "0.4.0")
        );
        assert_eq!(
            spread_delay(Duration::ZERO, &seed, "0.3.0"),
            Duration::ZERO,
            "GPN_UPDATE_SPREAD_SECS=0 disables the delay"
        );
    }

    /// 100 nodes that all find the same release in the same minute must not all
    /// start downloading in the same minute.
    #[test]
    fn a_hundred_nodes_spread_their_downloads() {
        let spread = spread_max(None);
        let mut buckets = std::collections::BTreeMap::<u64, u32>::new();
        for i in 0..100 {
            let seed = schedule_seed(&format!("account:release-day-{i}"));
            let d = spread_delay(spread, &seed, "0.3.0").as_secs();
            *buckets.entry(d / 60).or_default() += 1;
        }
        assert!(
            buckets.len() >= 15,
            "downloads must occupy most of the 20 one-minute slots, got {buckets:?}"
        );
        assert!(
            buckets.values().all(|n| *n <= 20),
            "no single minute may hold the whole fleet: {buckets:?}"
        );
    }

    // ── asset selection ─────────────────────────────────────────────────────

    #[test]
    fn selects_the_asset_for_the_platform() {
        let m = sample_manifest();
        assert!(select_asset(&m, "linux", "x86_64").is_ok());
        assert!(select_asset(&m, "linux", "amd64").is_ok(), "alias");
        match select_asset(&m, "windows", "x86_64") {
            Err(UpdateError::NoAsset { os, arch }) => {
                assert_eq!((os.as_str(), arch.as_str()), ("windows", "x86_64"));
            }
            other => panic!("expected NoAsset, got {other:?}"),
        }
    }

    // ── staging + swap ──────────────────────────────────────────────────────

    #[test]
    fn stage_then_swap_keeps_a_backup() {
        let dir = td("swap");
        let exe = dir.join("gpn_node");
        std::fs::write(&exe, b"old-binary").unwrap();
        let state = dir.join("state");

        let asset = sample_asset();
        let new_bytes = b"new-binary-bytes";
        let pending = stage_bytes(&state, &exe, "0.2.0", &asset, new_bytes).unwrap();
        assert_eq!(pending.version, "0.2.0");
        assert_eq!(
            std::fs::read(staged_path(&exe)).unwrap(),
            new_bytes,
            "staged file holds the new bytes"
        );
        assert_eq!(
            std::fs::read(&exe).unwrap(),
            b"old-binary",
            "staging must not touch the live binary"
        );

        let applied = apply_pending(&state, &exe)
            .unwrap()
            .unwrap()
            .expect("applied");
        assert_eq!(applied.version, "0.2.0");
        assert_eq!(std::fs::read(&exe).unwrap(), new_bytes, "swapped in");
        assert_eq!(
            std::fs::read(backup_path(&exe)).unwrap(),
            b"old-binary",
            "previous binary kept as .bak for rollback"
        );
        assert!(!staged_path(&exe).exists(), "staged file consumed");
        assert!(read_pending(&state).is_none(), "marker cleared");
        assert!(apply_pending(&state, &exe).unwrap().unwrap().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn staging_refuses_bytes_that_do_not_match_the_hash() {
        let dir = td("badhash");
        let exe = dir.join("gpn_node");
        std::fs::write(&exe, b"old-binary").unwrap();
        let state = dir.join("state");
        let asset = sample_asset(); // sha256 of "new-binary-bytes"

        let err = stage_bytes(&state, &exe, "0.2.0", &asset, b"evil-binary!").unwrap_err();
        assert!(matches!(err, UpdateError::BadDigest { .. }));
        assert!(!staged_path(&exe).exists(), "nothing staged");
        assert!(read_pending(&state).is_none(), "no marker");
        assert_eq!(std::fs::read(&exe).unwrap(), b"old-binary");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupting_the_staged_file_aborts_the_swap() {
        let dir = td("corrupt");
        let exe = dir.join("gpn_node");
        std::fs::write(&exe, b"old-binary").unwrap();
        let state = dir.join("state");

        let asset = sample_asset();
        stage_bytes(&state, &exe, "0.2.0", &asset, b"new-binary-bytes").unwrap();
        // Simulate bit-rot / a partial write between staging and restart.
        std::fs::write(staged_path(&exe), b"tampered-after-staging").unwrap();

        let err = apply_pending(&state, &exe).unwrap_err();
        assert!(matches!(err, UpdateError::BadDigest { .. }));
        assert_eq!(
            std::fs::read(&exe).unwrap(),
            b"old-binary",
            "a bad staged file must never be swapped in"
        );
        assert!(!staged_path(&exe).exists());
        assert!(read_pending(&state).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_marker_for_another_binary_is_dropped() {
        let dir = td("stale");
        let exe_a = dir.join("gpn_node-a");
        let exe_b = dir.join("gpn_node-b");
        std::fs::write(&exe_a, b"a").unwrap();
        std::fs::write(&exe_b, b"b").unwrap();
        let state = dir.join("state");

        let asset = sample_asset();
        stage_bytes(&state, &exe_a, "0.2.0", &asset, b"new-binary-bytes").unwrap();

        let out = apply_pending(&state, &exe_b).unwrap();
        assert!(
            out.unwrap().is_none(),
            "must not clobber a different binary"
        );
        assert_eq!(std::fs::read(&exe_b).unwrap(), b"b");
        assert!(read_pending(&state).is_none());
        // exe_a is untouched too — the marker was simply dropped.
        assert_eq!(std::fs::read(&exe_a).unwrap(), b"a");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_staged_is_a_no_op() {
        let dir = td("noop");
        let exe = dir.join("gpn_node");
        std::fs::write(&exe, b"old").unwrap();
        assert!(apply_pending(&dir.join("state"), &exe)
            .unwrap()
            .unwrap()
            .is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn staged_name_is_a_sibling_of_the_exe() {
        // The swap must be a same-directory rename (atomic on every OS), so the
        // staged file has to live next to the binary, not in /tmp.
        let exe = Path::new("/home/op/.local/bin/gpn_node");
        assert_eq!(
            staged_path(exe),
            PathBuf::from("/home/op/.local/bin/gpn_node.new")
        );
        assert_eq!(
            backup_path(exe),
            PathBuf::from("/home/op/.local/bin/gpn_node.bak")
        );
        assert_eq!(
            staged_path(Path::new("C:/gpn/gpn_node.exe")),
            PathBuf::from("C:/gpn/gpn_node.exe.new")
        );
    }

    // ── bundles ─────────────────────────────────────────────────────────────

    /// A fake gpn binary carrying the stamp `build.rs` injects.
    fn stamped_binary(version: &str) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"\x7fELF-padding-padding-padding");
        v.extend_from_slice(format!("gpn-build-v1 version=unknown sha=unknown\0").as_bytes());
        v.extend_from_slice(
            format!("gpn-build-v1 version={version} sha=deadbeef spec=41\0").as_bytes(),
        );
        v.extend_from_slice(b"more-code-here");
        v
    }

    /// Build a release-shaped `.tar.gz`: flat, both binaries, real stamps.
    fn make_bundle(dir: &Path, version: &str) -> PathBuf {
        use flate2::write::GzEncoder;
        let path = dir.join("gpn-linux.tar.gz");
        let f = std::fs::File::create(&path).unwrap();
        let enc = GzEncoder::new(f, flate2::Compression::fast());
        let mut ar = tar::Builder::new(enc);
        for name in [NODE_BIN, RUST_NODE_BIN] {
            let body = stamped_binary(version);
            let mut h = tar::Header::new_gnu();
            h.set_size(body.len() as u64);
            h.set_mode(0o755);
            h.set_cksum();
            ar.append_data(&mut h, format!("./{name}"), body.as_slice())
                .unwrap();
        }
        let mut h = tar::Header::new_gnu();
        h.set_size(5);
        h.set_mode(0o644);
        h.set_cksum();
        ar.append_data(&mut h, "./README", b"hello".as_slice())
            .unwrap();
        ar.into_inner().unwrap().finish().unwrap();
        path
    }

    fn read_bytes(p: &Path) -> Vec<u8> {
        std::fs::read(p).unwrap()
    }

    #[test]
    fn layout_is_inferred_from_the_url() {
        let mut a = sample_asset();
        assert_eq!(asset_layout(&a), LAYOUT_SINGLE);
        assert!(!is_bundle(&a));
        for url in [
            "https://x/gpn-linux.tar.gz",
            "https://x/gpn-linux.tgz?download=1",
            "https://x/gpn-windows.zip",
        ] {
            a.url = url.into();
            assert!(is_bundle(&a), "{url} must be a bundle");
        }
        // A declared layout can *enable* bundle handling for an extension-less
        // URL, but it can never downgrade a signed archive URL back to a single
        // binary — that would stage a 331 MB tarball as the new executable.
        a.url = "https://x/gpn_node".into();
        a.layout = " BUNDLE ".into();
        assert!(
            is_bundle(&a),
            "an explicit marker wins and is case-insensitive"
        );
        a.url = "https://x/gpn-linux.tar.gz".into();
        a.layout = LAYOUT_SINGLE.into();
        assert!(is_bundle(&a), "the signed URL wins over an unsigned marker");
    }

    #[test]
    fn layout_is_signed_but_backwards_compatible() {
        // No bundle fields -> the current rendering is byte-identical to the
        // pre-bundle one, so an old manifest keeps verifying.
        let m = sample_manifest();
        assert_eq!(m.canonical_text(), m.canonical_text_legacy());
        assert!(!String::from_utf8(m.canonical_text())
            .unwrap()
            .contains("layout="));

        let mut b = sample_manifest();
        b.assets[0].layout = LAYOUT_BUNDLE.into();
        b.assets[0].files.insert("gpn_node".into(), "ab".repeat(32));
        let text = String::from_utf8(b.canonical_text()).unwrap();
        assert_ne!(b.canonical_text(), b.canonical_text_legacy());
        assert!(text.contains(" layout=bundle\n"));
        assert!(text.contains(&format!("file=linux/x86_64 gpn_node {}\n", "ab".repeat(32))));

        // Stripping `layout` from a signed bundle manifest must NOT verify.
        let (sk, pk) = test_key();
        let body = signed_json(&b, &sk);
        let mut v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        v["assets"][0].as_object_mut().unwrap().remove("layout");
        assert_eq!(
            verify_manifest(&serde_json::to_vec(&v).unwrap(), &pk),
            Err(UpdateError::BadSignature),
            "layout is part of the signed bytes"
        );
    }

    #[test]
    fn a_layout_only_manifest_verifies_with_the_legacy_rendering() {
        // The shape 0.2.0 publishes: `layout` present in JSON, signature over the
        // legacy text, so pre-bundle nodes (which compute only that text) accept
        // it. New nodes must accept it too, via the same fallback.
        let (sk, pk) = test_key();
        let mut m = sample_manifest();
        m.assets[0].url = "https://x/gpn-linux.tar.gz".into();
        m.assets[0].layout = LAYOUT_BUNDLE.into();
        m.signing_key = hex::encode(sk.verifying_key().to_bytes());
        m.signature = hex::encode(sk.sign(&m.canonical_text_legacy()).to_bytes());
        let body = serde_json::to_vec(&m).unwrap();

        let got = verify_manifest(&body, &pk).expect("legacy-signed bundle manifest must verify");
        assert!(is_bundle(select_asset(&got, "linux", "x86_64").unwrap()));

        // ...and it is still tamper-proof: the signed fields are the URL/hash.
        let mut v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        v["assets"][0]["url"] = serde_json::json!("https://evil/gpn-linux.tar.gz");
        assert_eq!(
            verify_manifest(&serde_json::to_vec(&v).unwrap(), &pk),
            Err(UpdateError::BadSignature)
        );
    }

    #[test]
    fn bundle_manifest_verifies_and_selects_the_bundle() {
        let (sk, pk) = test_key();
        let dir = td("bundlemanifest");
        let bundle = make_bundle(&dir, "0.2.0");
        let mut m = sample_manifest();
        m.assets[0] = bundle_asset(
            "https://github.com/o/r/releases/download/linux-latest/gpn-linux.tar.gz",
            &read_bytes(&bundle),
        );
        m.assets[0]
            .files
            .insert(NODE_BIN.into(), sha256_hex(&stamped_binary("0.2.0")));
        let body = signed_json(&m, &sk);
        let got = verify_manifest(&body, &pk).expect("bundle manifest must verify");
        let a = select_asset(&got, "linux", "x86_64").unwrap();
        assert!(is_bundle(a));
        assert!(verify_file(&bundle, a).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bundle_download_extract_and_stamp_verification() {
        let dir = td("bundle-stage");
        let state = dir.join("update");
        let bundle = make_bundle(&dir, "0.2.0");
        let asset = bundle_asset("https://example/gpn-linux.tar.gz", &read_bytes(&bundle));

        let pending = stage_bundle_file(&state, "0.2.0", &asset, &bundle).unwrap();
        assert!(pending.is_bundle());
        assert_eq!(pending.version, "0.2.0");
        let vdir = version_dir(&state, "0.2.0");
        assert_eq!(pending.version_dir, vdir.display().to_string());
        assert!(node_bin(&vdir).is_file(), "supervisor present");
        assert!(vdir.join(RUST_NODE_BIN).is_file(), "node present");
        assert!(vdir.join("README").is_file(), "flat layout preserved");
        // No stray staging dirs left behind.
        let leftovers: Vec<_> = std::fs::read_dir(versions_root(&state))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
            .collect();
        assert!(leftovers.is_empty(), "temp dirs cleaned up");
        // `current` is untouched: staging is not installing.
        assert!(!current_link(&state).exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(node_bin(&vdir))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "binaries are executable");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bundle_with_the_wrong_build_stamp_is_rejected() {
        let dir = td("bundle-stamp");
        let state = dir.join("update");
        // The archive is 0.1.9, the manifest says 0.2.0.
        let bundle = make_bundle(&dir, "0.1.9");
        let asset = bundle_asset("https://example/gpn-linux.tar.gz", &read_bytes(&bundle));

        let err = stage_bundle_file(&state, "0.2.0", &asset, &bundle).unwrap_err();
        match err {
            UpdateError::BadStamp {
                expected, actual, ..
            } => {
                assert_eq!(expected, "0.2.0");
                assert_eq!(actual, "0.1.9");
            }
            other => panic!("expected BadStamp, got {other}"),
        }
        assert!(!version_dir(&state, "0.2.0").exists(), "nothing installed");
        assert!(read_pending(&state).is_none(), "no marker");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_bundle_hash_never_extracts_or_swaps() {
        let dir = td("bundle-badhash");
        let state = dir.join("update");
        let bundle = make_bundle(&dir, "0.2.0");
        let mut asset = bundle_asset("https://example/gpn-linux.tar.gz", &read_bytes(&bundle));
        asset.sha256 = sha256_hex(b"something-else");

        let err = stage_bundle_file(&state, "0.2.0", &asset, &bundle).unwrap_err();
        assert!(matches!(err, UpdateError::BadDigest { .. }));
        assert!(!versions_root(&state).exists() || !version_dir(&state, "0.2.0").exists());
        assert!(apply_pending(&state, Path::new("/bin/true"))
            .unwrap()
            .unwrap()
            .is_none());
        assert!(!current_link(&state).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bundle_per_file_hash_mismatch_is_rejected() {
        let dir = td("bundle-files");
        let state = dir.join("update");
        let bundle = make_bundle(&dir, "0.2.0");
        let mut asset = bundle_asset("https://example/gpn-linux.tar.gz", &read_bytes(&bundle));
        asset
            .files
            .insert("gpn-node".into(), sha256_hex(b"some-other-binary"));
        let err = stage_bundle_file(&state, "0.2.0", &asset, &bundle).unwrap_err();
        assert!(matches!(err, UpdateError::BadDigest { .. }));
        assert!(!version_dir(&state, "0.2.0").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_switch_and_one_step_rollback() {
        let dir = td("bundle-switch");
        let state = dir.join("update");

        // Install 0.2.0 (first install: nothing to roll back to).
        let b1 = make_bundle(&dir, "0.2.0");
        let a1 = bundle_asset("https://example/gpn-linux.tar.gz", &read_bytes(&b1));
        stage_bundle_file(&state, "0.2.0", &a1, &b1).unwrap();
        let first = apply_pending(&state, Path::new("/bin/true"))
            .unwrap()
            .unwrap()
            .expect("applied");
        assert_eq!(first.version, "0.2.0");
        assert_eq!(first.backup, PathBuf::new(), "no previous install");
        assert_eq!(installed_version(&state).as_deref(), Some("0.2.0"));
        assert_eq!(first.exec, node_bin(&version_dir(&state, "0.2.0")));
        assert!(read_pending(&state).is_none(), "marker cleared");

        // Then 0.3.0.
        let b2dir = td("bundle-switch-b");
        let b2 = make_bundle(&b2dir, "0.3.0");
        let a2 = bundle_asset("https://example/gpn-linux.tar.gz", &read_bytes(&b2));
        stage_bundle_file(&state, "0.3.0", &a2, &b2).unwrap();
        let second = apply_pending(&state, Path::new("/bin/true"))
            .unwrap()
            .unwrap()
            .expect("applied");
        assert_eq!(second.version, "0.3.0");
        assert_eq!(installed_version(&state).as_deref(), Some("0.3.0"));
        assert_eq!(
            second.backup,
            version_dir(&state, "0.2.0"),
            "the previous complete tree is kept, not deleted"
        );
        // Both trees survive the switch: 0.3.0 is live, 0.2.0 is the rollback.
        assert!(node_bin(&version_dir(&state, "0.2.0")).is_file());
        assert!(node_bin(&version_dir(&state, "0.3.0")).is_file());

        // The switch is a link flip: one atomic rename, no half-state.
        assert_eq!(
            current_version_dir(&state).unwrap(),
            version_dir(&state, "0.3.0")
        );

        // Roll back to 0.2.0, and forward again.
        let back = rollback_bundle(&state).unwrap().expect("rollback target");
        assert_eq!(back, version_dir(&state, "0.2.0"));
        assert_eq!(installed_version(&state).as_deref(), Some("0.2.0"));
        assert!(node_bin(&back).is_file(), "rollback tree intact");
        let fwd = rollback_bundle(&state).unwrap().expect("forward target");
        assert_eq!(fwd, version_dir(&state, "0.3.0"));

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&b2dir);
    }

    #[test]
    fn a_corrupted_version_dir_is_never_switched_to() {
        let dir = td("bundle-corrupt");
        let state = dir.join("update");
        let b = make_bundle(&dir, "0.2.0");
        let a = bundle_asset("https://example/gpn-linux.tar.gz", &read_bytes(&b));
        stage_bundle_file(&state, "0.2.0", &a, &b).unwrap();

        // Bit-rot / an operator edit between staging and the switch.
        std::fs::write(node_bin(&version_dir(&state, "0.2.0")), b"truncated").unwrap();
        let err = apply_pending(&state, Path::new("/bin/true")).unwrap_err();
        assert!(matches!(err, UpdateError::BadStamp { .. }));
        assert!(!current_link(&state).exists(), "current must not move");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pruning_keeps_current_and_the_rollback_target() {
        let dir = td("bundle-prune");
        let state = dir.join("update");
        for v in ["0.2.0", "0.3.0", "0.4.0"] {
            let bd = td(&format!("prune-{v}"));
            let b = make_bundle(&bd, v);
            let a = bundle_asset("https://example/gpn-linux.tar.gz", &read_bytes(&b));
            stage_bundle_file(&state, v, &a, &b).unwrap();
            apply_pending(&state, Path::new("/bin/true"))
                .unwrap()
                .unwrap()
                .expect("applied");
            let _ = std::fs::remove_dir_all(&bd);
        }
        assert!(version_dir(&state, "0.4.0").exists(), "current kept");
        assert!(version_dir(&state, "0.3.0").exists(), "rollback kept");
        assert!(
            !version_dir(&state, "0.2.0").exists(),
            "older versions are pruned so a node does not grow without bound"
        );
        assert_eq!(installed_version(&state).as_deref(), Some("0.4.0"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bundle_whose_stamps_disagree_is_rejected() {
        let dir = td("stamp-disagree");
        let exe = dir.join(node_bin_name());
        let node = dir.join(rust_node_bin_name());
        std::fs::write(&exe, stamped_binary("0.2.0")).unwrap();
        std::fs::write(&node, stamped_binary("0.1.9")).unwrap();
        match verify_bundle_stamps(&dir, "0.2.0") {
            Err(UpdateError::BadStamp { actual, .. }) => assert_eq!(actual, "0.1.9"),
            other => panic!("expected BadStamp, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_the_supervisor_needs_a_stamp_but_one_stamp_is_mandatory() {
        // What the real release looks like: build.rs is in gpn-agent, so only
        // gpn_node carries a stamp; the other ~60 binaries do not.
        let dir = td("stamp-shape");
        std::fs::write(dir.join(node_bin_name()), stamped_binary("0.2.0")).unwrap();
        std::fs::write(dir.join(rust_node_bin_name()), b"no stamp here").unwrap();
        std::fs::write(dir.join("gpn_scheduler"), b"no stamp here").unwrap();
        assert!(verify_bundle_stamps(&dir, "0.2.0").is_ok());

        // Nothing stamped at all: there is no evidence of what this tree is.
        std::fs::write(dir.join(node_bin_name()), b"no stamp here").unwrap();
        assert!(matches!(
            verify_bundle_stamps(&dir, "0.2.0"),
            Err(UpdateError::BadStamp { .. })
        ));

        // A missing gpn-node is not a GPN release.
        std::fs::write(dir.join(node_bin_name()), stamped_binary("0.2.0")).unwrap();
        std::fs::remove_file(dir.join(rust_node_bin_name())).unwrap();
        assert!(matches!(
            verify_bundle_stamps(&dir, "0.2.0"),
            Err(UpdateError::BundleShape(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Manual validation against a **real** release tree:
    ///
    /// ```bash
    /// tar -xzf gpn-linux.tar.gz -C /tmp/gpnstage
    /// GPN_REAL_BUNDLE_TREE=/tmp/gpnstage cargo test --lib real_release_tree_verifies -- --ignored
    /// ```
    #[test]
    #[ignore = "needs a real release tree (GPN_REAL_BUNDLE_TREE)"]
    fn real_release_tree_verifies() {
        let dir = std::env::var("GPN_REAL_BUNDLE_TREE")
            .expect("set GPN_REAL_BUNDLE_TREE to an unpacked release bundle");
        let version = std::env::var("GPN_REAL_BUNDLE_VERSION").unwrap_or_else(|_| "0.2.0".into());
        verify_bundle_stamps(Path::new(&dir), &version)
            .unwrap_or_else(|e| panic!("a real release tree must verify: {e}"));
    }

    /// The whole flow against the published 331 MB artifact: verify → extract →
    /// stamps → version dir → `current` flip → the switched-in binary reports the
    /// version the manifest named.
    ///
    /// ```bash
    /// GPN_REAL_BUNDLE_ARCHIVE=/tmp/gpn-linux.tar.gz \
    ///   cargo test --lib real_release_bundle_stages -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs the real release archive (GPN_REAL_BUNDLE_ARCHIVE)"]
    fn real_release_bundle_stages_and_switches() {
        let archive = std::env::var("GPN_REAL_BUNDLE_ARCHIVE")
            .expect("set GPN_REAL_BUNDLE_ARCHIVE to gpn-linux.tar.gz");
        let version = std::env::var("GPN_REAL_BUNDLE_VERSION").unwrap_or_else(|_| "0.2.0".into());
        let archive = PathBuf::from(archive);
        let bytes = std::fs::metadata(&archive).unwrap().len();
        let asset = ReleaseAsset {
            os: "linux".into(),
            arch: "x86_64".into(),
            url: "https://github.com/crewlyllc/gpn_coin/releases/download/linux-latest/gpn-linux.tar.gz"
                .into(),
            sha256: sha256_file(&archive).unwrap(),
            size: bytes,
            layout: LAYOUT_BUNDLE.into(),
            files: BTreeMap::new(),
        };
        let state = td("real-bundle");
        let pending = stage_bundle_file(&state, &version, &asset, &archive).expect("stage");
        assert_eq!(pending.version, version);
        let applied = apply_pending(&state, Path::new("/bin/true"))
            .unwrap()
            .unwrap()
            .expect("applied");
        assert_eq!(installed_version(&state).as_deref(), Some(version.as_str()));
        assert!(applied.exec.is_file());
        let out = std::process::Command::new(&applied.exec)
            .arg("--version")
            .output()
            .expect("the switched-in supervisor must run");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            text.contains(&version),
            "switched-in binary must report {version}: {text}"
        );
        eprintln!("switched-in: {}", text.trim());
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn a_single_layout_marker_still_uses_the_binary_swap() {
        let dir = td("single-marker");
        let exe = dir.join("gpn_node");
        std::fs::write(&exe, b"old-binary").unwrap();
        let state = dir.join("state");
        let mut asset = sample_asset();
        asset.layout = LAYOUT_SINGLE.into();
        stage_bytes(&state, &exe, "0.2.0", &asset, b"new-binary-bytes").unwrap();
        let applied = apply_pending(&state, &exe)
            .unwrap()
            .unwrap()
            .expect("applied");
        assert_eq!(applied.exec, exe);
        assert_eq!(read_bytes(&exe), b"new-binary-bytes");
        assert!(!current_link(&state).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── cached state ────────────────────────────────────────────────────────

    #[test]
    fn state_round_trips() {
        let dir = td("state");
        let st = UpdateState {
            last_check: 1234,
            latest_version: "0.2.0".into(),
            min_version: "0.1.5".into(),
            staged_version: None,
        };
        save_state(&dir, &st).unwrap();
        assert_eq!(load_state(&dir), Some(st));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── the manifest this repo actually publishes ───────────────────────────

    /// Cross-checks the real artifact: `website/releases.json`, as signed by
    /// `scripts/gen-release-manifest.py` (PyNaCl), must verify with the Rust
    /// verifier and the embedded key. This is the test that catches a signer /
    /// verifier drift in the canonical text format.
    #[test]
    fn published_manifest_verifies_with_the_embedded_key() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("website")
            .join("releases.json");
        let Ok(body) = std::fs::read(&path) else {
            eprintln!("skipping: {} not present", path.display());
            return;
        };
        let m = verify_manifest(&body, RELEASE_PUBKEY_HEX)
            .unwrap_or_else(|e| panic!("the published manifest must verify: {e}"));
        assert!(
            parse_version(&m.version).is_some(),
            "published version must be parseable: {}",
            m.version
        );
        assert!(
            parse_version(&m.min_version).is_some(),
            "published min_version must be parseable: {}",
            m.min_version
        );
        // The published release must be installable on both platforms, and a
        // bundle asset must be recognisable as one (otherwise the updater would
        // stage a 331 MB tarball as if it were a single binary).
        for (os, arch) in [("linux", "x86_64"), ("windows", "x86_64")] {
            match select_asset(&m, os, arch) {
                Ok(a) => {
                    assert!(a.size > 0, "{os}/{arch} must declare a size");
                    assert_eq!(a.sha256.len(), 64, "{os}/{arch} sha256");
                    if looks_like_bundle_url(&a.url) {
                        assert!(
                            is_bundle(a),
                            "{os}/{arch} points at an archive but is not a bundle"
                        );
                    }
                }
                Err(e) => panic!("the published manifest must cover {os}/{arch}: {e}"),
            }
        }
    }

    /// A two-asset manifest produced by the **Python** signer
    /// (`scripts/gen-release-manifest.py`) must verify with the Rust verifier.
    /// This is the format-drift guard: if either side changes the canonical
    /// text, this fails.
    #[test]
    fn fixture_signed_by_the_release_script_verifies() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("manifest-signed-multiplatform.json");
        let body = std::fs::read(&path).expect("fixture is committed");
        let m = verify_manifest(&body, RELEASE_PUBKEY_HEX).expect("fixture must verify");
        assert_eq!(m.version, "0.3.0");
        assert_eq!(m.min_version, "0.2.1");
        assert_eq!(m.assets.len(), 2);
        // Both platforms are selectable, and neither is a newer *node* concern.
        assert!(select_asset(&m, "linux", "x86_64").is_ok());
        assert!(select_asset(&m, "windows", "x86_64").is_ok());
        // The signed hashes are over the fixture's fake artifacts; a real
        // download of those bytes must match, anything else must not.
        assert!(verify_bytes(
            b"fake-linux-binary",
            select_asset(&m, "linux", "x86_64").unwrap()
        )
        .is_ok());
        assert!(verify_bytes(
            b"fake-win-binary!!",
            select_asset(&m, "windows", "x86_64").unwrap()
        )
        .is_ok());
        assert!(verify_bytes(b"evil", select_asset(&m, "linux", "x86_64").unwrap()).is_err());
    }

    // ── persisting the `--setup` answer ─────────────────────────────────────

    #[test]
    fn toml_bool_appends_when_absent() {
        let out = set_toml_bool("chain = \"testnet\"\n", "auto_update", true);
        assert_eq!(out, "chain = \"testnet\"\nauto_update = true\n");
    }

    #[test]
    fn toml_bool_replaces_in_place() {
        let src = "# comment\nauto_update = false\nstatus_secs = 15\n";
        let out = set_toml_bool(src, "auto_update", true);
        assert_eq!(out, "# comment\nauto_update = true\nstatus_secs = 15\n");
        assert!(out.contains("# comment"), "comments survive");
    }

    #[test]
    fn toml_bool_does_not_match_a_longer_key() {
        let out = set_toml_bool("update_auto_update = true\n", "auto_update", false);
        assert_eq!(
            out, "update_auto_update = true\nauto_update = false\n",
            "whole-key match only"
        );
    }

    #[test]
    fn toml_bool_collapses_duplicates() {
        let out = set_toml_bool(
            "auto_update = true\nauto_update = true\n",
            "auto_update",
            false,
        );
        assert_eq!(out, "auto_update = false\n");
    }

    #[test]
    fn toml_bool_output_parses_as_toml() {
        #[derive(serde::Deserialize)]
        struct T {
            auto_update: bool,
            chain: String,
        }
        let out = set_toml_bool(
            "chain = \"testnet\"\nauto_update = false\n",
            "auto_update",
            true,
        );
        let t: Result<T, _> = toml::from_str(&out);
        assert!(t.is_ok(), "must stay valid TOML: {out}");
        assert!(t.unwrap().auto_update);
    }
}
