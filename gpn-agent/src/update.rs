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
    /// Downloaded bytes do not match the manifest's `sha256`.
    BadDigest { expected: String, actual: String },
    /// Downloaded bytes are the wrong LENGTH for the manifest asset. Kept
    /// separate from `BadDigest` on purpose: a size mismatch and a hash
    /// mismatch are different facts, and telling an operator the wrong one
    /// sends them looking for the wrong cause (2026-11-03: a 321,604,470-byte
    /// download was reported as a "hash 321604470", and the manifest/asset
    /// disagreement was read as a corruption problem).
    BadSize { expected: u64, actual: u64 },
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
            UpdateError::BadSize { expected, actual } => write!(
                f,
                "downloaded file is {actual} bytes, but the manifest declares {expected} bytes \
                 — this is a SIZE mismatch (the published asset and releases.json disagree), \
                 not a corrupted hash; nothing was installed"
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

/// File name of the Rust node binary for the platform we are building for.
pub fn rust_node_bin_name() -> &'static str {
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

/// A pid-unique download path whose **extension** says how to extract it.
///
/// [`extract_bundle`] dispatches on the file name, so a staging file called
/// `…download` makes every bundle fail with "not a .tar.gz/.tar/.zip bundle".
/// Deriving the suffix from the signed asset URL keeps download and extraction
/// in agreement.
pub fn bundle_temp_path(state_dir: &Path, version: &str, asset: &ReleaseAsset) -> PathBuf {
    let url = asset.url.trim().to_ascii_lowercase();
    let path = url.split(['?', '#']).next().unwrap_or(&url);
    let ext = [".tar.gz", ".tgz", ".tar", ".zip"]
        .iter()
        .find(|e| path.ends_with(**e))
        .copied()
        .unwrap_or(".tar.gz");
    state_dir.join(format!(
        ".{}-bundle-{}{}",
        version.trim(),
        std::process::id(),
        ext
    ))
}

/// Does `versions/<version>` exist and look installed?
pub fn bundle_installed(state_dir: &Path, version: &str) -> bool {
    node_bin(&version_dir(state_dir, version)).is_file()
}

/// The version directory `current` points at, if any.
///
/// Two readers, because Windows has two link shapes and `read_link` is not the
/// same call for both:
///
/// * a **symlink** (Linux, and Windows Developer Mode) — `read_link` is exact;
/// * a **directory junction** (what `switch_current` creates on Windows, since
///   it needs no privilege) — a reparse point, which the Rust std walker may
///   refuse. `canonicalize` resolves the junction through the filesystem and is
///   the fallback. Without it, `current` reads as "no version": the updater
///   cannot name what is installed, `flip_to` records no rollback target, and
///   `prune_versions` would then delete the tree it was supposed to keep.
///
/// The result is only accepted when it is a real directory under
/// `versions/`, so a stray file called `current` cannot be mistaken for a
/// pointer.
pub fn current_version_dir(state_dir: &Path) -> Option<PathBuf> {
    let link = current_link(state_dir);
    let resolved = std::fs::read_link(&link)
        .ok()
        .map(|t| {
            if t.is_absolute() {
                t
            } else {
                state_dir.join(t)
            }
        })
        .or_else(|| std::fs::canonicalize(&link).ok())?;
    // `canonicalize` on Windows hands back a `\\?\` verbatim path; comparing
    // directory names works either way, but callers print it, so strip the
    // prefix when it is present.
    let dir = strip_verbatim(resolved);
    if !dir.is_dir() {
        return None;
    }
    // Only a tree the updater owns: the target's parent must be
    // `<state>/versions`. Compared after canonicalising both sides and again
    // as text (case-insensitively), because they come from different sources
    // (an env var and the filesystem) and `D:\gpn` vs `d:\gpn` must not read
    // as "a different tree".
    let root = versions_root(state_dir);
    let same_dir = |a: &Path, b: &Path| {
        let norm = |p: &Path| {
            p.display()
                .to_string()
                .replace('/', "\\")
                .trim_end_matches('\\')
                .to_ascii_lowercase()
        };
        norm(a) == norm(b)
    };
    let parent_ok = dir
        .parent()
        .map(|p| {
            same_dir(p, &root)
                || match (std::fs::canonicalize(p), std::fs::canonicalize(&root)) {
                    (Ok(pc), Ok(rc)) => same_dir(&pc, &rc),
                    _ => false,
                }
        })
        .unwrap_or(false);
    if !parent_ok {
        return None;
    }
    Some(dir)
}

/// Drop Windows' `\\?\` (and the UNC `\\?\UNC\`) prefix from a path.
fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.display().to_string();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!("\\\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    p
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

/// Default update-check cadence: **every 6 h**.
///
/// The 1 h value was the 2026-11-12 review window ("so a bad release is caught
/// the same day") and it was reverted in 0.3.59 (#161) — a temporary setting
/// with no revert date is how a debugging shortcut becomes permanent behaviour.
/// Operators can override either way with `GPN_UPDATE_CHECK_HOURS` (or
/// `update_check_hours` in the node TOML).
pub const DEFAULT_CHECK_HOURS: u64 = 6;

/// `GPN_UPDATE_CHECK_HOURS` → a duration, clamped to a sane range so a typo
/// cannot turn into a request storm (min 1 h) or an never-checking node.
pub fn check_interval(hours: Option<u64>) -> Duration {
    Duration::from_secs(hours.unwrap_or(DEFAULT_CHECK_HOURS).clamp(1, 168) * 3600)
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
        return Err(UpdateError::BadSize {
            expected: asset.size,
            actual: bytes.len() as u64,
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
            return Err(UpdateError::BadSize {
                expected: asset.size,
                actual: len,
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
        let applied = apply_pending_bundle(state_dir, &pending)?;
        if matches!(applied, Ok(Some(_))) {
            // The binaries just changed; the launcher that starts them may not
            // have. Repair it here, where every update passes — including on
            // boxes whose update wrapper predates the fix. The install dir is
            // derived from the state dir when it has the standard layout, and
            // from the calling binary otherwise.
            let install = install_dir_for_state(state_dir).or_else(|| {
                let bin = exe.parent()?;
                bin.parent().map(Path::to_path_buf)
            });
            if let Some(install) = install {
                if let Some(note) = repair_launcher_in_dir(&install) {
                    eprintln!("[update] {note}");
                }
            }
        }
        return Ok(applied);
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
    if pending.size > 0 && len != pending.size {
        discard_staged(state_dir, exe);
        return Err(UpdateError::BadSize {
            expected: pending.size,
            actual: len,
        });
    }
    if actual != pending.sha256 {
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
    Ok(read_build_stamp_full(path)?.map(|s| s.version))
}

/// A parsed `gpn-build-v1` stamp: the version *and* the commit the binary was
/// built from.
///
/// The commit matters because a version number alone cannot tell two builds of
/// the same tree apart — a host that deploys a locally built `HEAD` (version
/// `0.3.28`, sha `e188444`) and then lets the signed-release updater install
/// `0.3.28` (sha `0787c8b`) silently replaces newer code with older code and the
/// version comparison reports "up to date" the whole time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildStamp {
    /// `M.m.p`, exactly as embedded.
    pub version: String,
    /// The `sha=` field, when the build could name its commit.
    pub sha: Option<String>,
}

/// What the install guard decided about replacing the installed tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallVerdict {
    Allow,
    /// Refused, with a message that names both builds and the escape hatch.
    Refuse(String),
}

/// Should the signed release be allowed to replace the binaries in the install
/// dir?
///
/// Two things are refused, and both are silent today:
///
/// * a **downgrade** — the candidate version is older than what is installed;
/// * **the same version built from a different commit** — i.e. this host runs a
///   local build of that version and the release does not contain it. That is
///   the `#148` failure: `gpn-update.timer` replaced a `HEAD` deploy of 0.3.28
///   with the signed 0.3.28 cut of an *earlier* commit, and nothing noticed,
///   because `version == version`.
///
/// An **unknown** sha on either side is never treated as a mismatch (the
/// three-state rule: unknown is not evidence). No install at all, or a
/// candidate with no stamp, is allowed — the caller verifies stamps separately.
/// `force` is the operator's escape hatch for a deliberate re-install.
pub fn install_guard(
    installed: Option<&BuildStamp>,
    candidate: Option<&BuildStamp>,
    force: bool,
) -> InstallVerdict {
    if force {
        return InstallVerdict::Allow;
    }
    let (Some(installed), Some(candidate)) = (installed, candidate) else {
        return InstallVerdict::Allow;
    };
    match version_cmp(&candidate.version, &installed.version) {
        Some(Ordering::Less) => {
            return InstallVerdict::Refuse(format!(
                "refusing to downgrade {} -> {}: the install dir already runs a newer build{}. \
                 Re-run with --force if this is deliberate.",
                installed.version,
                candidate.version,
                described_sha(&installed.sha)
            ))
        }
        Some(Ordering::Equal) => {}
        // Unparseable versions are not evidence of anything.
        _ => return InstallVerdict::Allow,
    }
    match (&installed.sha, &candidate.sha) {
        (Some(a), Some(b)) if !a.eq_ignore_ascii_case(b) => InstallVerdict::Refuse(format!(
            "refusing to replace {} with a different build of the same version: the install \
             dir runs sha {}, the release was built from sha {}. Installing it would swap \
             newer code for older code and no version comparison could see it. \
             Re-run with --force if this is deliberate.",
            installed.version, a, b
        )),
        _ => InstallVerdict::Allow,
    }
}

fn described_sha(sha: &Option<String>) -> String {
    match sha {
        Some(s) => format!(" (sha {s})"),
        None => String::new(),
    }
}

/// Read the full stamp (version + commit) out of a binary.
pub fn read_build_stamp_full(path: &Path) -> Result<Option<BuildStamp>, UpdateError> {
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
                // The remaining fields sit in the same stamp string, right
                // after the version token.
                let sha = parse_stamp_sha(&window[at..]);
                return Ok(Some(BuildStamp { version: v, sha }));
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
        {
            let k = haystack[i..].iter().position(|b| *b == first)?;
            let at = i + k;
            if at + needle.len() > haystack.len() {
                return None;
            }
            if &haystack[at..at + needle.len()] == needle {
                return Some(at);
            }
            i = at + 1;
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

/// Parse the `sha=` field that follows the version token in the same stamp.
///
/// `build.rs` writes `sha=unknown` when it cannot see git history, so that value
/// is rejected like a missing field — "unknown commit" must not become a
/// mismatch that blocks a legitimate install.
fn parse_stamp_sha(tail: &[u8]) -> Option<String> {
    // Generous window: `version=X sha=Y spec=Z ` is far shorter than this.
    let window = &tail[..tail.len().min(96)];
    for tok in window.split(|b| b.is_ascii_whitespace() || *b == 0) {
        let Ok(tok) = std::str::from_utf8(tok) else {
            continue;
        };
        let Some(sha) = tok.strip_prefix("sha=") else {
            continue;
        };
        if sha.eq_ignore_ascii_case("unknown") || !looks_like_sha(sha) {
            return None;
        }
        return Some(sha.to_string());
    }
    None
}

fn looks_like_sha(s: &str) -> bool {
    (7..=40).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
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

/// Verify `versions/<version>` and make it `current`, keeping one rollback
/// pointer. Returns the version directory that was live **before** the flip.
///
/// This is the switch itself, shared by [`apply_pending_bundle`] (the staged
/// path the supervisor uses) and [`activate_version`] (the standalone updater
/// reusing a tree it already staged, e.g. after a crash in its copy step).
fn flip_to(state_dir: &Path, vdir: &Path, version: &str) -> Result<Option<PathBuf>, UpdateError> {
    verify_bundle_stamps(vdir, version)?;
    // `current` is the normal rollback source. A *recorded* pointer (written by
    // the standalone updater before the very first flip) is the fallback, so
    // pruning does not delete the only copy of the version being replaced.
    let previous = current_version_dir(state_dir).or_else(|| previous_pointer_dir(state_dir));
    switch_current(state_dir, vdir)?;
    if let Some(prev) = &previous {
        let _ = write_previous_pointer(state_dir, prev);
    }
    prune_versions(state_dir, vdir, previous.as_deref());
    Ok(previous)
}

/// Point `current` at an already-extracted, stamp-verified `versions/<version>`
/// without needing a `pending-update.json` marker.
///
/// Used by `gpn_update --apply` when the version tree is already on disk but
/// the marker was consumed (or the previous run died before the copy step).
pub fn activate_version(state_dir: &Path, version: &str) -> Result<PathBuf, UpdateError> {
    let vdir = version_dir(state_dir, version);
    if !node_bin(&vdir).is_file() {
        return Err(UpdateError::BundleShape(format!(
            "{} is not a complete version directory",
            vdir.display()
        )));
    }
    flip_to(state_dir, &vdir, version)?;
    Ok(vdir)
}

/// The version directory named in `current.previous`, for reporting.
pub fn previous_pointer_label(state_dir: &Path) -> Option<String> {
    let p = previous_pointer_dir(state_dir)?;
    Some(
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.display().to_string()),
    )
}

/// Record `dir` as the rollback target (`current.previous`).
///
/// Used by the standalone updater on a host that has no `current` yet: it
/// snapshots the install dir into `versions/<running>/` first, so the first
/// update still has a rollback target and is not a one-way door.
pub fn record_previous(state_dir: &Path, dir: &Path) -> Result<(), UpdateError> {
    write_previous_pointer(state_dir, dir)
}

/// The directory named by `current.previous`, if it is readable.
pub fn previous_pointer_dir(state_dir: &Path) -> Option<PathBuf> {
    let body = std::fs::read(previous_pointer(state_dir)).ok()?;
    let p = PathBuf::from(String::from_utf8_lossy(&body).trim().to_string());
    if p.as_os_str().is_empty() {
        return None;
    }
    Some(p)
}

// ---------------------------------------------------------------------------
// Launcher repair
// ---------------------------------------------------------------------------
//
// An update only swaps binaries, so every *other* file the installer wrote is
// frozen at whatever version first installed this box. Two of those files
// matter operationally:
//
//  * the node's command line: before 0.3.23 the launcher ran
//    `powershell -Command "& node 2>&1 | Tee-Object -FilePath <log> -Append"`,
//    which exits 1 *before the node starts* whenever the launcher is spawned
//    without a console. Box 1 on 2026-11-06: 0.3.23 on disk, dead in the water,
//    `last-exit.json` saying `version: launcher, pid: 0`.
//  * the seed list it pins: a launcher written before a seed address changed
//    keeps dialling the old one forever, so "auto-update repairs your node" was
//    false for exactly the machines that needed it (0.3.24 fixed line 1 only,
//    which is why a stale `GPN_BOOTNODES` survived every update since).
//
// Patching here (rather than in the launcher or the installer) is deliberate:
// the *binary* is the one thing an update always replaces, including on boxes
// whose update wrapper is still the old one.

/// Peer id of a `/p2p/<id>` multiaddr, if it has one.
fn multiaddr_peer_id(ma: &str) -> Option<&str> {
    let idx = ma.find("/p2p/")?;
    let id = ma[idx + "/p2p/".len()..].trim();
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

/// Split a comma-separated bootnode value, dropping empties, keeping order.
fn split_bootnodes(value: &str) -> Vec<String> {
    value
        .split([',', ';'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// True when every entry dials a peer id *we* publish — now or in the past.
///
/// This is the line between "a stale copy of our own defaults" (safe to refresh)
/// and "an operator pinned their own peers" (never touch). Anything we cannot
/// read a peer id out of counts as the operator's, so it is left alone.
///
/// Retired ids count as ours on purpose: a launcher that still carries a seed we
/// have stopped shipping is exactly the stale copy this repair exists for, and
/// treating it as foreign left the dead host in place (D11 residual).
fn is_our_seed_list(entries: &[String], canonical: &[String]) -> bool {
    let ours: std::collections::HashSet<&str> = canonical
        .iter()
        .map(String::as_str)
        .filter_map(multiaddr_peer_id)
        .chain(crate::chainspec::RETIRED_PEER_IDS.iter().copied())
        .collect();
    !entries.is_empty()
        && entries
            .iter()
            .all(|e| multiaddr_peer_id(e).is_some_and(|p| ours.contains(p)))
}

/// The value a stale seed-list line should carry, or `None` to leave it alone.
///
/// * empty → leave it: an empty `GPN_BOOTNODES` is meaningful (the node then
///   resolves `https://gpncoin.ai/seeds.json` at runtime — the freshest possible
///   answer, and nothing an update should overwrite).
/// * not ours → leave it: operator-supplied peers.
/// * same set as the current default → nothing to do (no write, no churn).
/// * ours but different → the stale case this exists for.
fn refreshed_seed_list(value: &str, canonical: &[String]) -> Option<String> {
    let entries = split_bootnodes(value);
    if entries.is_empty() || !is_our_seed_list(&entries, canonical) {
        return None;
    }
    let have: std::collections::BTreeSet<&str> = entries.iter().map(String::as_str).collect();
    let want: std::collections::BTreeSet<&str> = canonical.iter().map(String::as_str).collect();
    if have == want {
        return None;
    }
    Some(canonical.join(","))
}

/// `<value>` of `set "NAME=<value>"`, for a cmd launcher line.
fn set_line_value<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let t = line.trim();
    let lower = t.to_ascii_lowercase();
    let prefix = format!("set \"{}=", name.to_ascii_lowercase());
    if !lower.starts_with(&prefix) {
        return None;
    }
    t[prefix.len()..].strip_suffix('"')
}

/// `set "NAME=<value>"` → the same line re-quoted with a new value.
fn rewrite_set_line(line: &str, name: &str, value: &str) -> String {
    let indent = &line[..line.len() - line.trim_start().len()];
    format!("{indent}set \"{name}={value}\"")
}

fn between(s: &str, open: &str, close: char) -> Option<String> {
    let start = s.find(open)? + open.len();
    let rest = &s[start..];
    let end = rest.find(close)?;
    Some(rest[..end].to_string())
}

/// Rewrite a launcher body in place: every line that is *provably* stale is
/// repaired, everything else is preserved byte for byte.
///
/// Platform independent (and therefore unit-tested on every OS) — the Windows
/// half only supplies the file and the backup.
///
/// Returns the new body and one human-readable note per repair. An empty note
/// list means "nothing was stale": callers must not rewrite the file at all.
pub fn refresh_launcher_body(body: &str, canonical: &[String]) -> (String, Vec<String>) {
    refresh_launcher_body_with_install(body, canonical, None)
}

/// [`refresh_launcher_body`], plus the Windows-layout rewrite that makes the
/// launcher run the updater-selected version tree instead of the locked
/// `bin` copy.
///
/// `install_dir` is `Some(C:\gpn)` on an installed Windows host. Only the exact
/// `set "GPN_BIN=<install>\bin"` line is rewritten; a launcher that already
/// points at `data\update\current`, or has no such line, is left alone.
pub fn refresh_launcher_body_with_install(
    body: &str,
    canonical: &[String],
    install_dir: Option<&Path>,
) -> (String, Vec<String>) {
    let mut notes: Vec<String> = Vec::new();
    let mut out: Vec<String> = Vec::new();
    let crlf = body.contains("\r\n");

    for line in body.lines() {
        let trimmed = line.trim_start();

        // (1) The PowerShell pipeline that exits 1 before the node starts.
        if trimmed.starts_with("powershell") && line.contains("Tee-Object") {
            // Reuse the paths the old line already carries: never invent them.
            match (
                between(line, "& '", '\''),
                between(line, "FilePath '", '\''),
            ) {
                (Some(node), Some(log)) => {
                    out.push(format!("\"{node}\" >> \"{log}\" 2>&1"));
                    notes.push(
                        "the node is now exec'd directly (no PowerShell pipeline)".to_string(),
                    );
                    continue;
                }
                _ => {
                    // Cannot parse it safely: leave the line alone rather than
                    // write a launcher with a wrong path.
                    eprintln!(
                        "[update] launcher still wraps the node in a PowerShell pipeline, but its \
                         command line could not be parsed — left unchanged: {line}"
                    );
                }
            }
        }

        // (2) The launcher starts `bin\gpn_node.exe` directly, which is the
        // locked file an update cannot replace while the node runs. Point it at
        // the versioned tree the updater flips, keeping bin as the fallback.
        if let Some(install) = install_dir {
            if let Some(value) = bin_dir_value(line) {
                if same_path(&value, &install.join("bin")) {
                    for l in versioned_bin_lines(install) {
                        out.push(l);
                    }
                    notes.push(format!(
                        "the launcher now runs {}\\data\\update\\{CURRENT_LINK} \
                         (falls back to {}\\bin)",
                        install.display(),
                        install.display()
                    ));
                    continue;
                }
            }

            // A hard-coded exec line is the same defect one step later: the
            // release workflow's launcher ran `"C:\gpn\bin\gpn_node.exe"`
            // literally, so repairing GPN_BIN alone changed nothing — the
            // supervisor, and every child it spawns, still came from bin
            // (box 1, 2026-11-14).
            let literal = format!(
                "\"{}\\bin\\gpn_node.exe\"",
                install.display().to_string().trim_end_matches(['\\', '/'])
            );
            if trimmed.starts_with(&literal) {
                let rest = &trimmed[literal.len()..];
                out.push(format!("\"%GPN_BIN%\\gpn_node.exe\"{rest}"));
                notes.push(
                    "the supervisor is started through %GPN_BIN%, not a hard-coded bin path"
                        .to_string(),
                );
                continue;
            }
        }

        // (3) A stale seed list pinned by an older installer.
        let mut replaced = false;
        for name in ["GPN_BOOTNODES", "GPN_RESERVED_PEERS"] {
            let Some(value) = set_line_value(line, name) else {
                continue;
            };
            if let Some(new) = refreshed_seed_list(value, canonical) {
                let before = split_bootnodes(value).len();
                out.push(rewrite_set_line(line, name, &new));
                notes.push(format!(
                    "{name} refreshed: {before} stale entr{} -> {} canonical",
                    if before == 1 { "y" } else { "ies" },
                    canonical.len()
                ));
                replaced = true;
            }
            break;
        }
        if replaced {
            continue;
        }

        out.push(line.to_string());
    }

    if notes.is_empty() {
        return (body.to_string(), notes);
    }

    let mut text = out.join(if crlf { "\r\n" } else { "\n" });
    if body.ends_with('\n') {
        text.push_str(if crlf { "\r\n" } else { "\n" });
    }
    (text, notes)
}

/// Repair `start-node.cmd` next to the installed binary.
///
/// Returns a human-readable note when a repair happened, so `update.log` records
/// it. One backup, one write, however many stale lines there are: a launcher is
/// the one file that decides whether the machine starts at all, so the file is
/// replaced atomically-ish (`.bak-<ts>` then a single write) rather than patched
/// line by line.
#[cfg(windows)]
fn repair_windows_launcher(exe: &std::path::Path) -> Option<String> {
    let bin = exe.parent()?;
    let install = bin.parent()?;
    repair_launcher_in_dir(install)
}

/// Repair `start-node.cmd` inside `install`, whatever binary triggered it.
///
/// Separated from [`repair_windows_launcher`] because the process that notices
/// the need to repair is not always installed in `<install>/bin` — an update
/// applied from `data\update\current` would otherwise derive the wrong parent
/// and edit nothing.
#[cfg(windows)]
pub fn repair_launcher_in_dir(install: &Path) -> Option<String> {
    let launcher = install.join("start-node.cmd");
    let body = std::fs::read_to_string(&launcher).ok()?;
    let (out, notes) = refresh_launcher_body_with_install(
        &body,
        &crate::chainspec::launcher_bootnodes(),
        Some(install),
    );
    if notes.is_empty() {
        return None;
    }

    let backup = install.join(format!(
        "start-node.cmd.bak-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    ));
    let _ = std::fs::copy(&launcher, &backup);
    match std::fs::write(&launcher, out) {
        Ok(()) => Some(format!(
            "repaired start-node.cmd: {}; previous copy at {}",
            notes.join("; "),
            backup.display()
        )),
        Err(e) => {
            eprintln!("[update] could not rewrite {}: {e}", launcher.display());
            None
        }
    }
}

#[cfg(not(windows))]
#[allow(dead_code)] // only the Windows half calls it; kept so both cfgs compile alike
fn repair_windows_launcher(_exe: &std::path::Path) -> Option<String> {
    None
}

/// No-op off Windows: the launcher (and the locked-executable problem) is a
/// Windows shape. Returns `None` so callers treat it as "nothing to do".
#[cfg(not(windows))]
pub fn repair_launcher_in_dir(_install: &Path) -> Option<String> {
    None
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

    let previous = match flip_to(state_dir, &vdir, &pending.version) {
        Ok(prev) => prev,
        Err(e) => {
            // `current` still points at the old complete tree: nothing
            // half-swapped.
            return Ok(Err(SwapDeferred {
                version: pending.version.clone(),
                staged: vdir,
                reason: e.to_string(),
            }));
        }
    };
    let _ = std::fs::remove_file(pending_path(state_dir));

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
// Confirmation: "the release is running", not "the release is on disk"
// ─────────────────────────────────────────────────────────────────────────────
//
// 2026-11-14 (box 3): `versions/0.3.46` was staged and stamp-verified while the
// node kept running 0.3.45 for eight hours. Every layer reported success —
// staging verified the bytes, the flip moved `current`, the copy reported the
// files it wrote — and nothing asked the one question that matters: *is the
// process that is running that build?* These helpers close that gap.

/// Compare the version a running node reports with the one we installed.
///
/// Exact equality, not `>=`: after a rollback the old build is running, and a
/// `>=`-style check would call a half-finished upgrade a success. An answer
/// that does not parse is never a match — "I could not read the version" must
/// not be indistinguishable from "it is the right version".
pub fn version_matches(expected: &str, reported: &str) -> bool {
    match (parse_version(expected), parse_version(reported)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The confirm gate: three ways a start can fail, told apart (#207)
// ─────────────────────────────────────────────────────────────────────────────
//
// 2026-11-18 (a fifth box, not ours):
// `auto-update: confirm the new version failed (exit 1) - not confirmed within
// 360 s`. The swap had run and the confirmation had not succeeded — and the
// message could not say whether the node was starting slowly, up without RPC,
// or dead, nor which build was running when the report was written. A gate that
// cannot tell "slow" from "dead" rolls back correct work and hides real
// failures. The gate therefore watches two signals separately:
//
//   * `process` — an executable running out of the *expected version tree*,
//                 matched by path (the family has many children, so an image
//                 name alone proves nothing);
//   * `rpc`     — `system_version` answering, and what it answers.
//
// The three outcomes:
//   A. slow start   — process up, rpc not answering yet. Wait (up to the hard
//                     ceiling) and succeed the moment the version arrives.
//   B. rpc down     — process up for the whole ceiling, rpc never confirmed.
//                     Report "process up, RPC down" and fail *after* the wait.
//   C. process died — the process exited before confirming. Fail immediately;
//                     waiting six minutes to say "it is dead" helps nobody.

/// How long a *warm* restart gets before the gate starts reporting "process up,
/// RPC down". Not a deadline: the search continues to
/// [`CONFIRM_HARD_CEILING_SECS`].
pub const CONFIRM_WARM_SECS: u64 = 120;

/// How long the first start after an update gets. The first boot of a new build
/// rebuilds the DAG and starts syncing, which is why this is measured in
/// minutes, not seconds (box 1, 2026-11-17: JSON-RPC bound 125 s after start,
/// inside the old 360 s window, but a 120 s window had already rolled it back).
pub const CONFIRM_FIRST_START_SECS: u64 = 900;

/// The gate never waits longer than this, whatever window was requested. A
/// machine stuck behind a gate is as unavailable as one whose update failed.
pub const CONFIRM_HARD_CEILING_SECS: u64 = 900;

/// Grace before "I have never seen the process" is called a death. A restart
/// command may still be spawning; declaring the node dead at t=0 would turn
/// that into a rollback.
pub const CONFIRM_START_GRACE_SECS: u64 = 30;

/// Delay between polls. Short enough to notice a version that arrives, long
/// enough that ~450 polls over 900 s do not flood the node with RPC.
pub const CONFIRM_POLL_SECS: u64 = 2;

/// The two deadlines the gate works to.
///
/// `window_secs` is when "still starting" becomes "process up, RPC down";
/// `ceiling_secs` is when the gate gives up and the caller rolls back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfirmDeadlines {
    pub window_secs: u64,
    pub ceiling_secs: u64,
}

impl ConfirmDeadlines {
    /// Apply an operator's `--timeout` / env override.
    ///
    /// The override is a budget for the whole wait, so it replaces **both**
    /// deadlines: window and ceiling are clamped to `secs`, and the hard
    /// ceiling still caps it. An override can shorten the wait, never extend it
    /// past [`CONFIRM_HARD_CEILING_SECS`].
    ///
    /// Clamping only the window was a real defect (2026-11-18, CI run
    /// 44399271742): `--timeout 1` set the window to 1 s but left the ceiling at
    /// 900 s, so `--confirm` with nothing listening gave up after 900.8 s — the
    /// opposite of the bounded-time contract this gate exists to provide.
    pub fn with_window(self, secs: u64) -> Self {
        let secs = secs.min(self.ceiling_secs);
        ConfirmDeadlines {
            window_secs: secs,
            ceiling_secs: secs,
        }
    }
}

/// Deadlines for a confirm: `first_start_after_update` chooses the window, the
/// ceiling is always the hard one.
pub fn confirm_deadlines(first_start_after_update: bool) -> ConfirmDeadlines {
    ConfirmDeadlines {
        window_secs: if first_start_after_update {
            CONFIRM_FIRST_START_SECS
        } else {
            CONFIRM_WARM_SECS
        },
        ceiling_secs: CONFIRM_HARD_CEILING_SECS,
    }
}

/// What the child said on its way out (#230).
///
/// `wait_for_confirm` used to report only *that* the process was gone. On a
/// fifth machine 0.3.65 said `signal=process (never appeared) … no process ever
/// appeared` while the child had already recorded exit 69 `no-ollama` — a named
/// cause the operator never saw. Every field is optional on purpose: a probe
/// that cannot read a source must say nothing rather than guess.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChildEvidence {
    /// The child's process exit code, when the exit ledger recorded one.
    pub exit_code: Option<i32>,
    /// The stable exit kind slug (`no-ollama`, `corrupt-key`, `no-gpu`, …).
    pub kind: Option<String>,
    /// The child's own one-line explanation, verbatim.
    pub detail: Option<String>,
    /// The first line the child wrote after this attempt began.
    ///
    /// The launcher redirects the node's stdout **and** stderr into `node.log`
    /// (`gpn_node.exe >> node.log 2>&1`), so this is the first thing the child
    /// said on either stream, not a stderr-only capture. Named honestly.
    pub first_output: Option<String>,
    /// The last line of the node log — where the child stopped.
    pub log_last: Option<String>,
}

impl ChildEvidence {
    /// True when there is nothing to report.
    pub fn is_empty(&self) -> bool {
        self.exit_code.is_none()
            && self.kind.is_none()
            && self.detail.is_none()
            && self.first_output.is_none()
            && self.log_last.is_none()
    }

    /// The one-line suffix an operator reads. Empty when there is no evidence.
    pub fn summary(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut parts: Vec<String> = Vec::new();
        match (self.exit_code, self.kind.as_deref()) {
            (Some(c), Some(k)) => parts.push(format!("exit {c} ({k})")),
            (Some(c), None) => parts.push(format!("exit {c}")),
            (None, Some(k)) => parts.push(format!("exit kind {k}")),
            (None, None) => {}
        }
        if let Some(d) = self.detail.as_deref() {
            if !d.trim().is_empty() {
                parts.push(d.trim().to_string());
            }
        }
        if let Some(s) = self.first_output.as_deref() {
            if !s.trim().is_empty() {
                parts.push(format!("first output: {}", s.trim()));
            }
        }
        if let Some(l) = self.log_last.as_deref() {
            if !l.trim().is_empty() {
                parts.push(format!("node log: {}", l.trim()));
            }
        }
        parts.join("; ")
    }

    /// The same facts as a JSON object, for `last-exit.json`.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "exit_code": self.exit_code,
            "kind": self.kind,
            "detail": self.detail,
            "first_output": self.first_output,
            "log_last": self.log_last,
        })
    }
}

/// What the two probes look like to the gate. A trait (rather than direct RPC
/// and `System` calls) so the three scenarios are testable in microseconds
/// instead of fifteen minutes of real waiting.
pub trait ConfirmProbe {
    /// Is a process running out of the expected version tree?
    ///
    /// `None` means "cannot tell" — never "dead": a box whose process could
    /// not be inspected must not be rolled back on that basis alone.
    fn process_alive(&mut self) -> Option<bool>;
    /// Ask the running node for `system_version`. `Err` carries why not.
    fn rpc_version(&mut self) -> Result<String, String>;
    /// Seconds since the gate started.
    fn elapsed_secs(&mut self) -> u64;
    /// Advance the clock (sleep in production, nothing under test).
    fn wait(&mut self, secs: u64);

    /// Positive evidence that security software stopped the build (#209).
    ///
    /// Default `None`: a probe that cannot tell must never turn a dead process
    /// into "blocked by security software".
    fn security_block(&mut self) -> Option<String> {
        None
    }

    /// What the child process said when it died (#230).
    ///
    /// Default `None`: a probe that cannot read the exit ledger or the node log
    /// must report nothing rather than invent a cause.
    fn child_evidence(&mut self) -> Option<ChildEvidence> {
        None
    }
}

/// Why the gate gave up. Each names the signal that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmFailure {
    /// C: the process exited (or never appeared) before it confirmed.
    ProcessGone {
        at_secs: u64,
        /// "exited" or "never appeared" — the operator needs the difference.
        detail: &'static str,
        last_rpc: String,
        /// #230: the child's own account of why it is gone, when one exists.
        child: Option<ChildEvidence>,
    },
    /// B: the process stayed up but `system_version` never confirmed the build
    /// — no answer at all, or an answer that was not the expected version.
    RpcDown {
        at_secs: u64,
        /// `None` when process liveness could not be determined.
        process_up: Option<bool>,
        last_rpc: String,
    },
    /// D (#209): the build never got to run because security software stopped
    /// it — the file was quarantined, or the loader was refused access.
    ///
    /// Chosen **only** on positive evidence (see [`detect_security_block`]); a
    /// node that merely failed to start is still [`ProcessGone`]. This exists so
    /// the operator is told "blocked by security software" instead of a generic
    /// timeout for a cause that no retry can fix.
    BlockedBySecuritySoftware { at_secs: u64, evidence: String },
}

impl ConfirmFailure {
    /// Which signal failed: `"process"`, `"rpc"` or `"security"`.
    pub fn signal(&self) -> &'static str {
        match self {
            ConfirmFailure::ProcessGone { .. } => "process",
            ConfirmFailure::RpcDown { .. } => "rpc",
            ConfirmFailure::BlockedBySecuritySoftware { .. } => "security",
        }
    }

    pub fn at_secs(&self) -> u64 {
        match self {
            ConfirmFailure::ProcessGone { at_secs, .. } => *at_secs,
            ConfirmFailure::RpcDown { at_secs, .. } => *at_secs,
            ConfirmFailure::BlockedBySecuritySoftware { at_secs, .. } => *at_secs,
        }
    }

    /// The child's evidence, when the failing signal was the process itself.
    pub fn child_evidence(&self) -> Option<&ChildEvidence> {
        match self {
            ConfirmFailure::ProcessGone { child, .. } => child.as_ref(),
            _ => None,
        }
    }

    /// The one line an operator reads. Names the failing signal, how long the
    /// gate waited, and what the other signal was doing.
    pub fn describe(&self, expected: &str) -> String {
        match self {
            ConfirmFailure::ProcessGone {
                at_secs,
                detail,
                last_rpc,
                child,
            } => {
                let base = format!(
                    "confirm failed: signal=process ({detail}) after {at_secs} s — {expected} never came up; last rpc: {last_rpc}"
                );
                // #230: "never appeared" without a cause is not actionable. Add
                // what the child itself recorded, and nothing when it is silent.
                match child.as_ref().map(ChildEvidence::summary) {
                    Some(s) if !s.is_empty() => format!("{base}; child: {s}"),
                    _ => base,
                }
            }
            ConfirmFailure::RpcDown {
                at_secs,
                process_up,
                last_rpc,
            } => {
                let proc_txt = match process_up {
                    Some(true) => "process up, RPC down",
                    Some(false) => "process down, RPC down",
                    None => "process state unknown, RPC down",
                };
                format!(
                    "confirm failed: signal=rpc ({proc_txt}) after {at_secs} s — {expected} not confirmed; last rpc: {last_rpc}"
                )
            }
            ConfirmFailure::BlockedBySecuritySoftware { at_secs, evidence } => format!(
                "confirm failed: signal=security (blocked by security software) after {at_secs} s — \
                 {expected} was stopped before it could run: {evidence}; this is not a timeout and a \
                 retry will not fix it — exclude {} and {} from antivirus/Defender scanning \
                 (install.ps1 asks before doing this, or run fix-defender.ps1 -Consent yes)",
                SECURITY_EXCLUSION_PATHS[0], SECURITY_EXCLUSION_PATHS[1]
            ),
        }
    }
}

/// The two stable paths a Windows install excludes from security scanning
/// (#209). File-level on purpose: a directory exclusion trusts anything that
/// lands in a writable folder.
pub const SECURITY_EXCLUSION_PATHS: [&str; 2] = [
    r"C:\gpn\bin\gpn-node.exe",
    r"C:\gpn\data\update\current\gpn-node.exe",
];

/// NTSTATUS `STATUS_ACCESS_DENIED` — the loader was refused permission to map
/// the image. The signature of an antivirus/policy block.
pub const STATUS_ACCESS_DENIED: u32 = 0xC000_0022;
/// NTSTATUS `STATUS_DLL_NOT_FOUND`. Ambiguous on its own (a missing VC++
/// runtime also produces it), so it only counts as security evidence when the
/// text also names security software.
pub const STATUS_DLL_NOT_FOUND: u32 = 0xC000_0135;

/// Does this text name security software, a quarantine, or a policy block?
/// Deliberately specific: "access is denied" alone is not enough (a file lock
/// says that too).
pub fn mentions_security_software(text: &str) -> bool {
    let low = text.to_ascii_lowercase();
    [
        "virus",
        "threat",
        "quarantin",
        "antivirus",
        "anti-virus",
        "security software",
        "windows defender",
        "blocked by policy",
        "operation did not complete successfully",
        "this program is blocked",
        "0xc0000022",
    ]
    .iter()
    .any(|n| low.contains(n))
}

/// The exit code a Windows image left, out of a `last-exit.json` record. The
/// record stores it signed (Rust `i64`) or unsigned, so both are accepted.
fn exit_code_from_last_exit(last_exit: &str) -> Option<i64> {
    let line = last_exit.lines().rev().find(|l| !l.trim().is_empty())?;
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    for key in ["exit_code", "code", "status"] {
        if let Some(n) = v.get(key).and_then(|c| c.as_i64()) {
            return Some(n);
        }
        if let Some(s) = v.get(key).and_then(|c| c.as_str()) {
            if let Some(h) = s.trim().strip_prefix("0x") {
                if let Ok(n) = i64::from_str_radix(h, 16) {
                    return Some(n);
                }
            }
            if let Ok(n) = s.trim().parse::<i64>() {
                return Some(n);
            }
        }
    }
    None
}

fn exit_code_says_access_denied(code: i64) -> bool {
    code as u32 == STATUS_ACCESS_DENIED
}

fn exit_code_says_dll_not_found(code: i64) -> bool {
    code as u32 == STATUS_DLL_NOT_FOUND
}

/// Positive evidence that security software stopped the build from starting.
///
/// Evidence, in order of reliability:
/// 1. the binary that should be there is **gone** while its directory exists —
///    what a quarantine does (it deletes the file it blocked);
/// 2. the exit record carries `0xC0000022` (access denied to the image);
/// 3. `0xC0000135` **plus** a security-software phrase in the same text.
///
/// Everything else returns `None`. Guessing here would put "blocked by
/// security software" on every ordinary crash, which is worse than the timeout
/// this replaces. `exists` is injected so the rule is testable without Windows.
pub fn detect_security_block(
    expected_exes: &[PathBuf],
    exists: &dyn Fn(&Path) -> bool,
    last_exit: &str,
) -> Option<String> {
    let mut reasons: Vec<String> = Vec::new();

    // 0. The evidence text itself names the block (Defender/PowerShell wording,
    //    e.g. "Operation did not complete successfully because the file
    //    contains a virus or potentially unwanted software").
    if mentions_security_software(last_exit) {
        let line = last_exit
            .lines()
            .rev()
            .find(|l| mentions_security_software(l))
            .unwrap_or("")
            .trim()
            .to_string();
        reasons.push(format!("the exit evidence says: {line}"));
    }

    // 1. A quarantined file is deleted. Only a path whose parent directory is
    //    present counts: "never installed" is not evidence of a block.
    for exe in expected_exes {
        let parent_exists = exe.parent().map(exists).unwrap_or(false);
        if parent_exists && !exists(exe.as_path()) {
            reasons.push(format!("{} is missing from its directory", exe.display()));
        }
    }

    // 2/3. The exit code, from the newest `last-exit.json` record.
    if let Some(code) = exit_code_from_last_exit(last_exit) {
        if exit_code_says_access_denied(code) {
            reasons.push(format!(
                "the loader refused access to the image (exit 0x{:08X} STATUS_ACCESS_DENIED)",
                code as u32
            ));
        } else if exit_code_says_dll_not_found(code) && mentions_security_software(last_exit) {
            reasons.push(format!(
                "the image could not load (exit 0x{:08X}) and the exit record names security software", code as u32
            ));
        }
    }

    if reasons.is_empty() {
        None
    } else {
        Some(reasons.join("; "))
    }
}

/// The gate's answer: the version it observed, or why it stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmOutcome {
    Confirmed { version: String, at_secs: u64 },
    Failed(ConfirmFailure),
}

fn alive_label(alive: Option<bool>) -> &'static str {
    match alive {
        Some(true) => "up",
        Some(false) => "down",
        None => "unknown",
    }
}

/// `last_rpc` for a report: the last thing the endpoint said, or the fact that
/// it said nothing at all.
fn rpc_hint(last: &str) -> String {
    if last.trim().is_empty() {
        "no answer yet".to_string()
    } else {
        last.to_string()
    }
}

/// Poll both signals until `expected` is running, the process dies, or the
/// ceiling passes. Pure apart from the probe and the log callback.
///
/// Logs one line each time the (process, rpc) state changes, e.g.
/// `confirm: process=up rpc=no 45s` and
/// `confirm: process=up rpc=yes 92s version=0.3.59`.
pub fn wait_for_confirm(
    probe: &mut dyn ConfirmProbe,
    expected: &str,
    deadlines: ConfirmDeadlines,
    mut log: impl FnMut(&str),
) -> ConfirmOutcome {
    let mut saw_alive = false;
    let mut last_rpc = String::new();
    let mut logged: Option<(Option<bool>, bool)> = None;
    let mut warned_past_window = false;

    loop {
        let alive = probe.process_alive();
        if alive == Some(true) {
            saw_alive = true;
        }
        let rpc = probe.rpc_version();
        let elapsed = probe.elapsed_secs();
        // "RPC answered" includes answering with the wrong version: the port is
        // bound and the process is speaking, which is the distinction that
        // matters here. The version question is what `last_rpc` records.
        let rpc_ok = rpc.is_ok();

        match &rpc {
            Ok(v) => {
                // An answer proves something is listening, whatever its version.
                saw_alive = true;
                if version_matches(expected, v) {
                    log(&format!(
                        "confirm: process={} rpc=yes {elapsed}s version={v}",
                        alive_label(alive)
                    ));
                    return ConfirmOutcome::Confirmed {
                        version: v.clone(),
                        at_secs: elapsed,
                    };
                }
                last_rpc = format!("the node reports {v}, expected {expected}");
            }
            Err(e) => last_rpc = e.clone(),
        }

        // C: it was alive and now it is not. Hard failure, immediate.
        if saw_alive && alive == Some(false) {
            // #209: before calling this an ordinary death, ask the probe for
            // positive evidence of a security-software block. Only positive
            // evidence is accepted; `None` keeps the plain ProcessGone path.
            if let Some(evidence) = probe.security_block() {
                log(&format!(
                    "confirm: process=down rpc=no {elapsed}s — blocked by security software: {evidence}"
                ));
                return ConfirmOutcome::Failed(ConfirmFailure::BlockedBySecuritySoftware {
                    at_secs: elapsed,
                    evidence,
                });
            }
            let child = probe.child_evidence().filter(|c| !c.is_empty());
            log(&format!(
                "confirm: process=down rpc=no {elapsed}s — the process exited before it confirmed{}",
                child
                    .as_ref()
                    .map(|c| format!(" ({})", c.summary()))
                    .unwrap_or_default()
            ));
            return ConfirmOutcome::Failed(ConfirmFailure::ProcessGone {
                at_secs: elapsed,
                detail: "exited",
                last_rpc: rpc_hint(&last_rpc),
                child,
            });
        }
        // C, the other half: it never appeared at all.
        if !saw_alive && alive == Some(false) && elapsed >= CONFIRM_START_GRACE_SECS {
            if let Some(evidence) = probe.security_block() {
                log(&format!(
                    "confirm: process=down rpc=no {elapsed}s — blocked by security software: {evidence}"
                ));
                return ConfirmOutcome::Failed(ConfirmFailure::BlockedBySecuritySoftware {
                    at_secs: elapsed,
                    evidence,
                });
            }
            let child = probe.child_evidence().filter(|c| !c.is_empty());
            log(&format!(
                "confirm: process=down rpc=no {elapsed}s — no process in the expected version tree{}",
                child
                    .as_ref()
                    .map(|c| format!(" ({})", c.summary()))
                    .unwrap_or_default()
            ));
            return ConfirmOutcome::Failed(ConfirmFailure::ProcessGone {
                at_secs: elapsed,
                detail: "never appeared",
                last_rpc: rpc_hint(&last_rpc),
                child,
            });
        }

        let state = (alive, rpc_ok);
        if logged != Some(state) {
            let version = match &rpc {
                Ok(v) => format!(" version={v}"),
                Err(_) => String::new(),
            };
            log(&format!(
                "confirm: process={} rpc={} {elapsed}s{version}",
                alive_label(alive),
                if rpc_ok { "yes" } else { "no" }
            ));
            logged = Some(state);
        }

        // B: past the window with the process up but no confirmation. Say so
        // once, then keep polling to the hard ceiling.
        if !warned_past_window && elapsed >= deadlines.window_secs {
            warned_past_window = true;
            log(&format!(
                "confirm: process={} rpc={} {elapsed}s — past the {}s window, waiting to the {}s ceiling",
                alive_label(alive),
                if rpc_ok { "yes" } else { "no" },
                deadlines.window_secs,
                deadlines.ceiling_secs
            ));
        }

        if elapsed >= deadlines.ceiling_secs {
            log(&format!(
                "confirm: giving up at {elapsed}s (ceiling {}s)",
                deadlines.ceiling_secs
            ));
            // #209: a node that never appeared, or whose liveness cannot be
            // read, may be a blocked image. Only when the process is NOT known
            // to be up: an RPC failure beside a running node is not a block.
            if alive != Some(true) && !saw_alive {
                if let Some(evidence) = probe.security_block() {
                    log(&format!(
                        "confirm: blocked by security software at the ceiling: {evidence}"
                    ));
                    return ConfirmOutcome::Failed(ConfirmFailure::BlockedBySecuritySoftware {
                        at_secs: elapsed,
                        evidence,
                    });
                }
            }
            return ConfirmOutcome::Failed(ConfirmFailure::RpcDown {
                at_secs: elapsed,
                process_up: alive.or(if saw_alive { Some(true) } else { None }),
                last_rpc: rpc_hint(&last_rpc),
            });
        }

        probe.wait(CONFIRM_POLL_SECS);
    }
}

/// Where the attempt ledger lives.
pub const ATTEMPTS_FILE: &str = "update-attempts.json";

// ─────────────────────────────────────────────────────────────────────────────
// Remediation: a failure is a thing to fix, not only to report (#207)
// ─────────────────────────────────────────────────────────────────────────────
//
// 2026-11-18 (owner correction): distinguishing the three scenarios is not the
// fix. B (RPC never binds) and C (process died) both have known causes on this
// fleet, and every one of them has a mechanical response. The gate therefore
// classifies a start failure from the node's own evidence — `last-exit.json`
// and the tail of `node.log` — and applies **at most one** remediation + retry
// per update. If the retry fails the caller rolls back; nothing loops.

/// The most remediation attempts one update gets. Exactly one, by rule: an
/// updater that keeps trying is an updater that hides a broken release.
pub const MAX_REMEDIATION_ATTEMPTS: u32 = 1;

/// Everything the classifier may look at.
#[derive(Debug, Clone, Copy)]
pub struct StartFailureContext<'a> {
    /// `last-exit.json` (NDJSON; the last line is the newest record), or "".
    pub last_exit: &'a str,
    /// The last few lines of the node's log, verbatim.
    pub log_tail: &'a [String],
}

impl<'a> StartFailureContext<'a> {
    fn text(&self) -> String {
        let mut s = self.last_exit.to_string();
        for l in self.log_tail {
            s.push('\n');
            s.push_str(l);
        }
        s
    }
}

/// What to do about a failure that has a known cause, or a considered refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Remediation {
    /// An orphaned `gpn*` child still holds the DB or the log file. The
    /// provider/AI children are *not* named `gpn-agent`, so the whole family has
    /// to be found by name prefix and stopped children-first.
    OrphanedFamily { cause: String },
    /// The swapped DB's pruning mode disagrees with the configured one. The
    /// supervisor re-resolves pruning per spawn; the retry must not pin a mode.
    PruningMismatch { stored: String, cause: String },
    /// The RPC port is held by one of *our* processes from an earlier run: stop
    /// it (children first) and retry.
    PortHeldByGpn {
        pid: Option<u32>,
        name: String,
        port: u16,
    },
    /// RPC port held by something that is not ours. Never fight it.
    PortHeldByForeign {
        pid: Option<u32>,
        name: String,
        port: u16,
    },
    /// A missing/corrupt keystore or node-key: no heroic repair. Roll back.
    FatalConfig { cause: String },
    /// #209: security software quarantined or refused the image. Never
    /// actionable — a retry re-runs the same blocked file. Named so the
    /// operator's report says what happened instead of "unrecognised exit".
    BlockedBySecuritySoftware { cause: String },
    /// Everything else: roll back and quote the evidence.
    Unknown { detail: String },
}

impl Remediation {
    /// `Some` when applying it has a chance of working without a human. A
    /// foreign process on the RPC port is deliberately *not* actionable: two
    /// services fighting over a port is worse than one clean failure.
    pub fn is_actionable(&self) -> bool {
        matches!(
            self,
            Remediation::OrphanedFamily { .. }
                | Remediation::PruningMismatch { .. }
                | Remediation::PortHeldByGpn { .. }
        )
    }

    /// The `remediate:` line in the log: `<cause> -> <action> -> retry`.
    pub fn log_line(&self) -> String {
        match self {
            Remediation::OrphanedFamily { cause } => format!(
                "remediate: {cause} -> stop the orphaned gpn family children-first, then retry"
            ),
            Remediation::PruningMismatch { stored, cause } => format!(
                "remediate: {cause} -> let the supervisor re-resolve pruning per spawn (DB stores {stored}), then retry"
            ),
            Remediation::PortHeldByGpn { pid, name, port } => format!(
                "remediate: rpc port {port} held by {name}{} -> stop the stale gpn family (children-first), retry",
                pid.map(|p| format!(" (pid {p})")).unwrap_or_default()
            ),
            Remediation::PortHeldByForeign { pid, name, port } => format!(
                "remediate: rpc port {port} is held by {name}{} -> not fighting it; report and roll back",
                pid.map(|p| format!(" (pid {p})")).unwrap_or_default()
            ),
            Remediation::FatalConfig { cause } => format!(
                "remediate: {cause} -> no automatic repair; report the exact file and roll back"
            ),
            Remediation::BlockedBySecuritySoftware { cause } => format!(
                "remediate: blocked by security software ({cause}) -> a retry re-runs the same blocked \
                 file; exclude {} and {} from antivirus/Defender scanning and roll back",
                SECURITY_EXCLUSION_PATHS[0], SECURITY_EXCLUSION_PATHS[1]
            ),
            Remediation::Unknown { detail } => format!(
                "remediate: unrecognised exit ({detail}) -> roll back and quote the last log lines"
            ),
        }
    }
}

/// Is this process name part of the gpn family? Prefix, because the tree has
/// `gpn_node`, `gpn-node`, `gpn_update` and friends, and the child that holds a
/// lock is often not the one the operator started.
pub fn is_gpn_process_name(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    n.starts_with("gpn")
}

/// The stored pruning mode out of a Substrate error such as
/// `Incompatible pruning modes [stored: Constrained(256); requested: Archive]`.
/// Returns `"256"` / `"archive"` — what the retry may ask for.
pub fn parse_stored_pruning(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let at = lower.find("stored:")?;
    let rest = &text[at + "stored:".len()..];
    let end = rest.find([']', ';', '\n']).unwrap_or(rest.len());
    let field = rest[..end].trim();
    let mode = if let (Some(a), Some(b)) = (field.find('('), field.rfind(')')) {
        field[a + 1..b].trim().to_string()
    } else {
        field.to_ascii_lowercase()
    };
    if mode.is_empty() {
        return None;
    }
    // Normalise the names Substrate prints to the ones its CLI accepts.
    let mode = match mode.as_str() {
        "archiveall" | "archive-all" => "archive".to_string(),
        "constrained" => "256".to_string(),
        other => other.to_string(),
    };
    Some(mode)
}

/// Who holds the RPC port, and what to do about it. `None` owner = could not
/// tell, which is treated as foreign: killing an unidentified process is how an
/// updater takes a machine down.
pub fn classify_port_holder(port: u16, owner: Option<(Option<u32>, String)>) -> Remediation {
    match owner {
        Some((pid, name)) if is_gpn_process_name(&name) => {
            Remediation::PortHeldByGpn { pid, name, port }
        }
        Some((pid, name)) => Remediation::PortHeldByForeign { pid, name, port },
        None => Remediation::PortHeldByForeign {
            pid: None,
            name: "unidentified process".to_string(),
            port,
        },
    }
}

/// Does this text say the RPC port could not be bound?
pub fn log_says_port_conflict(text: &str) -> bool {
    let low = text.to_ascii_lowercase();
    low.contains("address already in use")
        || low.contains("address in use")
        || low.contains("only one usage of each socket address")
        || low.contains("failed to bind")
}

/// Classify a failed start from the node's own evidence.
///
/// Order matters: the lock error is the most common cause on this fleet and the
/// most cheaply fixed; a pruning mismatch names itself; a keystore problem is
/// the one we must *not* "fix".
pub fn classify_start_failure(ctx: &StartFailureContext<'_>) -> Remediation {
    let text = ctx.text();
    let lower = text.to_ascii_lowercase();

    let lock = lower.contains("used by another process")
        || lower.contains("lockfile")
        || lower.contains("cannot access the file")
        || lower.contains("os error 32")
        || lower.contains("sharing violation");
    if lock {
        return Remediation::OrphanedFamily {
            cause: "lock held by another process".to_string(),
        };
    }

    // #209: an antivirus/Defender block looks like an ordinary death unless the
    // evidence says otherwise. Checked before pruning/key because the block's
    // text can name anything (it quotes the file it removed).
    if let Some(cause) = detect_security_block(&[], &|_| true, &text) {
        return Remediation::BlockedBySecuritySoftware { cause };
    }

    if lower.contains("incompatible pruning modes") {
        return Remediation::PruningMismatch {
            stored: parse_stored_pruning(&text).unwrap_or_else(|| "unknown".to_string()),
            cause: "incompatible pruning modes".to_string(),
        };
    }

    let key = lower.contains("node-key")
        || lower.contains("keystore")
        || lower.contains("corrupt-key")
        || lower.contains("invalid seed")
        || lower.contains("invalid phrase");
    if key {
        let detail = first_matching_line(ctx, &["node-key", "keystore", "corrupt", "invalid"])
            .unwrap_or_else(|| "keystore or node-key is missing or unusable".to_string());
        return Remediation::FatalConfig {
            cause: detail.trim().to_string(),
        };
    }

    let detail = if let Some(l) = ctx.log_tail.last() {
        l.trim().to_string()
    } else {
        last_exit_summary(ctx.last_exit).unwrap_or_else(|| "no evidence recorded".to_string())
    };
    Remediation::Unknown { detail }
}

/// The gate plus **one** remediation and retry (#207).
///
/// Pure with respect to the world: the probes, the classification and the
/// corrective action are all injected, so the four required tests run in
/// microseconds instead of fifteen minutes each. The rule it enforces is
/// [`MAX_REMEDIATION_ATTEMPTS`]: at most one remediation per update, never a
/// loop, and a failed retry is returned to the caller, which rolls back.
pub fn confirm_with_remediation(
    make_probe: &mut dyn FnMut() -> Box<dyn ConfirmProbe>,
    classify: &mut dyn FnMut() -> Remediation,
    apply: &mut dyn FnMut(&Remediation) -> Result<(), String>,
    expected: &str,
    deadlines: ConfirmDeadlines,
    log: &mut dyn FnMut(&str),
) -> Result<String, String> {
    let mut remediations = 0u32;
    let mut tried: Vec<String> = Vec::new();
    loop {
        let mut probe = make_probe();
        match wait_for_confirm(probe.as_mut(), expected, deadlines, &mut *log) {
            ConfirmOutcome::Confirmed { version, .. } => return Ok(version),
            ConfirmOutcome::Failed(failure) => {
                let why = failure.describe(expected);
                log(&why);
                if remediations >= MAX_REMEDIATION_ATTEMPTS {
                    let suffix = if tried.is_empty() {
                        String::new()
                    } else {
                        format!("; already tried: {}", tried.join(", "))
                    };
                    return Err(format!("{why}{suffix}"));
                }
                let remediation = classify();
                let line = remediation.log_line();
                log(&line);
                if !remediation.is_actionable() {
                    return Err(format!("{why}; {line}"));
                }
                apply(&remediation)?;
                tried.push(line);
                remediations += 1;
                log(&format!(
                    "remediate: retrying (attempt {remediations}/{MAX_REMEDIATION_ATTEMPTS})"
                ));
            }
        }
    }
}

fn first_matching_line(ctx: &StartFailureContext<'_>, needles: &[&str]) -> Option<String> {
    ctx.log_tail
        .iter()
        .map(|s| s.as_str())
        .chain(ctx.last_exit.lines())
        .find(|l| {
            let low = l.to_ascii_lowercase();
            needles.iter().any(|n| low.contains(n))
        })
        .map(|l| l.trim().to_string())
}

/// `kind` + `detail` from the newest `last-exit.json` record, if it parses.
pub fn last_exit_summary(last_exit: &str) -> Option<String> {
    let line = last_exit.lines().rev().find(|l| !l.trim().is_empty())?;
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let kind = v.get("kind").and_then(|k| k.as_str()).unwrap_or("?");
    let detail = v.get("detail").and_then(|d| d.as_str()).unwrap_or("");
    Some(if detail.trim().is_empty() {
        format!("last exit: {kind}")
    } else {
        format!("last exit: {kind} — {detail}")
    })
}

/// The last `n` non-empty lines, verbatim — what the operator's report quotes.
pub fn log_tail(text: &str, n: usize) -> Vec<String> {
    let mut lines: Vec<String> = text
        .lines()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .take(n)
        .map(|l| l.to_string())
        .collect();
    lines.reverse();
    lines
}

/// The TCP port a JSON-RPC URL points at (`http://127.0.0.1:9944` -> 9944).
pub fn rpc_port(url: &str) -> Option<u16> {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let hostport = rest.split('/').next().unwrap_or(rest);
    let port = hostport.rsplit(':').next()?;
    port.parse::<u16>().ok()
}

/// Retry ladder after a failed attempt: 1 h, 2 h, then 4 h for every further
/// attempt (#203). A node never gives up — a permanent stop is how box 1 sat
/// two releases behind on 2026-11-16 — and the ladder only keeps a genuinely
/// broken release from being re-downloaded on every single hourly tick.
///
/// The timer cadence (one check per hour) is the floor on how often an attempt
/// can happen at all: `next_retry_at` can delay a tick, never add one.
pub const RETRY_BACKOFF_SECS: [u64; 3] = [3_600, 7_200, 14_400];

/// Delay before the next attempt after `attempts` failures (1-based).
pub fn retry_backoff_secs(attempts: u32) -> u64 {
    let i = attempts.saturating_sub(1) as usize;
    RETRY_BACKOFF_SECS[i.min(RETRY_BACKOFF_SECS.len() - 1)]
}

/// An install was started for this version but no outcome has been recorded.
pub const OUTCOME_STARTED: &str = "started";
/// The new build came up and reported the installed version.
pub const OUTCOME_CONFIRMED: &str = "confirmed";
/// The binaries were installed but nothing was restarted (a hand-run
/// `--apply`), so there is nothing to confirm yet.
pub const OUTCOME_INSTALLED: &str = "installed";
/// The new build did not confirm, `current` was flipped back and the previous
/// build was started again.
pub const OUTCOME_ROLLED_BACK: &str = "rolled-back";
/// The install or the restart failed outright.
pub const OUTCOME_FAILED: &str = "failed";

/// How many attempts are kept. One entry per release is all that matters, and a
/// node updates a few dozen times in its life; the cap stops a pathological
/// retry loop from growing the file without bound.
const MAX_ATTEMPTS: usize = 64;

/// One recorded install attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateAttempt {
    pub version: String,
    /// Identity of the boot this attempt happened in (see [`boot_id`]).
    pub boot_id: String,
    /// Unix seconds.
    #[serde(default)]
    pub at: u64,
    /// One of the `OUTCOME_*` constants.
    pub outcome: String,
    /// Identity of the *release* that was attempted — the asset sha256 from the
    /// signed manifest.
    ///
    /// This is what lets a rolled-back attempt re-arm without waiting for a
    /// reboot: a version can be re-cut (same `0.3.52`, different bytes), and a
    /// box that gave up on the old build must still try the new one. Empty for
    /// ledgers written before this field existed, and then it blocks as it
    /// always did.
    #[serde(default)]
    pub release_id: String,
    /// How many attempts this version has had (across boots). Drives the
    /// backoff ladder; 0 on ledgers written before #203.
    #[serde(default)]
    pub attempts: u32,
    /// Why the last attempt failed, in the operator's words.
    #[serde(default)]
    pub reason: String,
    /// Unix seconds before which the next attempt must not be made. 0 means
    /// "no delay recorded" (a success, or an old ledger): try again now.
    #[serde(default)]
    pub next_retry_at: u64,
}

/// `update-attempts.json`: the small history behind the retry rule.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptLedger {
    #[serde(default)]
    pub attempts: Vec<UpdateAttempt>,
}

pub fn attempts_path(state_dir: &Path) -> PathBuf {
    state_dir.join(ATTEMPTS_FILE)
}

/// Read the ledger; an unreadable/corrupt file is an empty ledger (the guard
/// must never be the reason a node refuses to update).
pub fn load_attempts(state_dir: &Path) -> AttemptLedger {
    std::fs::read(attempts_path(state_dir))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_attempts(state_dir: &Path, l: &AttemptLedger) -> Result<(), UpdateError> {
    std::fs::create_dir_all(state_dir)?;
    let body = serde_json::to_vec_pretty(l).map_err(|e| UpdateError::Io(e.to_string()))?;
    let target = attempts_path(state_dir);
    let tmp = PathBuf::from(format!("{}.tmp-{}", target.display(), std::process::id()));
    std::fs::write(&tmp, &body)?;
    std::fs::rename(&tmp, &target)?;
    Ok(())
}

/// Choose the boot identity from its two sources (pure, so it is testable).
///
/// `GPN_BOOT_ID` wins: on Windows the scheduled-task wrapper knows the machine's
/// boot time and passes it, because there is no `/proc` to read.
pub fn pick_boot_id(env_value: Option<&str>, proc_boot_id: Option<&str>) -> String {
    for v in [env_value, proc_boot_id].into_iter().flatten() {
        let v = v.trim();
        if !v.is_empty() {
            return v.to_string();
        }
    }
    // Unknown is honest: the attempt is still recorded, but it cannot be used to
    // block a retry (see `attempt_blocks`). Blocking on an identity we do not
    // have would strand a machine on a broken release forever.
    "unknown".to_string()
}

/// The identity of the current boot: `GPN_BOOT_ID`, else the kernel's boot id.
pub fn boot_id() -> String {
    let env = std::env::var("GPN_BOOT_ID").ok();
    #[cfg(target_os = "linux")]
    let proc_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok();
    #[cfg(not(target_os = "linux"))]
    let proc_id: Option<String> = None;
    pick_boot_id(env.as_deref(), proc_id.as_deref())
}

/// The attempt recorded for `version` on this boot, if any.
pub fn attempt_for_this_boot(
    state_dir: &Path,
    version: &str,
    boot_id: &str,
) -> Option<UpdateAttempt> {
    load_attempts(state_dir)
        .attempts
        .into_iter()
        .rev()
        .find(|a| a.version.trim() == version.trim() && a.boot_id == boot_id)
}

/// Does a recorded attempt forbid trying again on this boot?
///
/// Only a *failed* or *rolled-back* attempt does — those are outcomes the
/// confirm step actually produced. `started` deliberately does **not** block
/// (#207): an attempt is consumed by the confirm step, so a run that died
/// before confirming (crash, power loss, updater killed) stays retryable.
/// `confirmed`/`installed` are the successful outcomes.
///
/// An `unknown` boot id never blocks: we cannot tell "same boot" from "new
/// boot", and treating them as the same would exile a box from every future
/// update.
pub fn attempt_blocks(prev: &UpdateAttempt) -> bool {
    if prev.boot_id == "unknown" {
        return false;
    }
    matches!(prev.outcome.as_str(), OUTCOME_ROLLED_BACK | OUTCOME_FAILED)
}

/// Does a recorded attempt forbid trying *this release* again on this boot?
///
/// Same rule as [`attempt_blocks`], minus one case: if the failed attempt was
/// made against a **different build of the same version**, the attempt no
/// longer applies. A publisher who re-cuts `0.3.52` with the fix (the normal
/// answer to "the new build rolled back") must not need every box rebooted
/// before it can take it.
///
/// Both ids empty ⇒ an old ledger, or a caller with no release identity: keep
/// the historical blocking behaviour rather than inventing a re-arm.
pub fn attempt_blocks_release(prev: &UpdateAttempt, release_id: &str) -> bool {
    if !attempt_blocks(prev) {
        return false;
    }
    let (had, now) = (prev.release_id.trim(), release_id.trim());
    if had.is_empty() || now.is_empty() {
        return true;
    }
    had == now
}

/// The newest recorded attempt for `version`, whatever boot it happened in.
///
/// #203: retry state must survive a reboot. Looking a failure up *by boot* was
/// how a box could fail, reboot, fail, reboot and never make progress while the
/// log said "already attempted on this boot".
pub fn attempt_for_version(state_dir: &Path, version: &str) -> Option<UpdateAttempt> {
    load_attempts(state_dir)
        .attempts
        .into_iter()
        .rev()
        .find(|a| a.version.trim() == version.trim())
}

/// How many times `version` has been attempted, across every boot.
pub fn attempts_recorded(state_dir: &Path, version: &str) -> u32 {
    load_attempts(state_dir)
        .attempts
        .iter()
        .filter(|a| a.version.trim() == version.trim())
        .map(|a| a.attempts.max(1))
        .max()
        .unwrap_or(0)
}

/// The newest record in the ledger, any version.
pub fn latest_attempt(state_dir: &Path) -> Option<UpdateAttempt> {
    load_attempts(state_dir)
        .attempts
        .into_iter()
        .max_by_key(|a| a.at)
}

/// How long the caller must still wait before retrying *this release*.
///
/// * `None` — nothing blocks it (the last attempt was for different bytes, or
///   the last outcome was a success): install now.
/// * `Some(0)` — the last attempt failed but its backoff has elapsed: retry
///   now, on this tick.
/// * `Some(n)` — wait `n` more seconds. `n` is bounded by the backoff ladder,
///   never by "the rest of this boot".
pub fn retry_wait_secs(prev: &UpdateAttempt, release_id: &str, now: u64) -> Option<u64> {
    if !attempt_blocks_release(prev, release_id) {
        return None;
    }
    Some(prev.next_retry_at.saturating_sub(now))
}

/// The operator-facing line for a declined retry, used by the log, the CLI and
/// `/status` so all three say the same words.
pub fn declined_text(next_retry_at: u64, reason: &str) -> String {
    let reason = if reason.trim().is_empty() {
        "no reason recorded"
    } else {
        reason.trim()
    };
    format!(
        "update declined until {} (last: {reason})",
        format_unix_utc(next_retry_at)
    )
}

/// `2026-11-17 15:00:00 UTC` — no chrono in this crate, and a wrong timestamp
/// is worse than a plain one, so the civil-date conversion is here and tested.
pub fn format_unix_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, mi, s) = (rem / 3_600, (rem % 3_600) / 60, rem % 60);
    // Howard Hinnant's civil_from_days, shifted to the Unix epoch.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02} UTC")
}

/// What `/status` may publish about a delayed retry (#203).
pub struct UpdateRetryView {
    pub version: String,
    pub next_retry_at: u64,
    pub last_reason: String,
    pub attempts: u32,
    pub last_attempt_at: u64,
}

/// The retry state to publish, or `None` when the last attempt did not fail.
///
/// A success is the newest record after a good update, so a healthy box
/// publishes nothing and its `/status` stays byte-identical.
pub fn update_retry_view(state_dir: &Path) -> Option<UpdateRetryView> {
    let a = latest_attempt(state_dir)?;
    if !attempt_blocks(&a) {
        return None;
    }
    Some(UpdateRetryView {
        version: a.version,
        next_retry_at: a.next_retry_at,
        last_reason: a.reason,
        attempts: a.attempts.max(1),
        last_attempt_at: a.at,
    })
}

/// Record (or replace) the attempt for `version` on `boot_id`.
pub fn record_attempt(
    state_dir: &Path,
    version: &str,
    boot_id: &str,
    outcome: &str,
) -> Result<(), UpdateError> {
    record_attempt_outcome(state_dir, version, boot_id, outcome, "", "").map(|_| ())
}

/// [`record_attempt`] that also records which build of the version was tried.
pub fn record_attempt_with_release(
    state_dir: &Path,
    version: &str,
    boot_id: &str,
    outcome: &str,
    release_id: &str,
) -> Result<(), UpdateError> {
    record_attempt_outcome(state_dir, version, boot_id, outcome, release_id, "").map(|_| ())
}

/// Record an attempt with its outcome and, for a failure, why it failed.
///
/// Returns the stored record, whose `attempts` and `next_retry_at` are what the
/// retry decision (and `/status`) read. A success clears the retry clock; a
/// failure sets it `retry_backoff_secs(attempts)` into the future — the ladder
/// is the *only* thing that ever stops a retry, and only until its next tick.
pub fn record_attempt_outcome(
    state_dir: &Path,
    version: &str,
    boot_id: &str,
    outcome: &str,
    release_id: &str,
    reason: &str,
) -> Result<UpdateAttempt, UpdateError> {
    let mut ledger = load_attempts(state_dir);
    // Count this version's attempts across boots *before* replacing our own
    // record, so the ladder keeps growing even for a box that reboots.
    let prior = ledger
        .attempts
        .iter()
        .filter(|a| a.version.trim() == version.trim())
        .map(|a| a.attempts.max(1))
        .max()
        .unwrap_or(0);
    // One attempt produces several records (`started` then the outcome). A
    // record written on top of an unfinished `started` continues that attempt;
    // anything else is the next rung of the ladder.
    let latest = ledger
        .attempts
        .iter()
        .filter(|a| a.version.trim() == version.trim())
        .max_by_key(|a| a.at)
        .map(|a| a.outcome.clone());
    let attempts = if prior == 0 {
        1
    } else if latest.as_deref() == Some(OUTCOME_STARTED) {
        prior
    } else {
        prior.saturating_add(1)
    };
    let next_retry_at = if outcome == OUTCOME_STARTED
        || outcome == OUTCOME_FAILED
        || outcome == OUTCOME_ROLLED_BACK
    {
        now_unix().saturating_add(retry_backoff_secs(attempts))
    } else {
        0
    };
    let record = UpdateAttempt {
        version: version.trim().to_string(),
        boot_id: boot_id.to_string(),
        release_id: release_id.trim().to_string(),
        at: now_unix(),
        outcome: outcome.to_string(),
        attempts,
        reason: reason.trim().to_string(),
        next_retry_at,
    };
    ledger
        .attempts
        .retain(|a| !(a.version.trim() == version.trim() && a.boot_id == boot_id));
    ledger.attempts.push(record.clone());
    if ledger.attempts.len() > MAX_ATTEMPTS {
        let drop = ledger.attempts.len() - MAX_ATTEMPTS;
        ledger.attempts.drain(0..drop);
    }
    save_attempts(state_dir, &ledger)?;
    Ok(record)
}

/// The install directory a state dir belongs to, for the launcher repair.
///
/// The standard layout is `<install>/data/update`, so the install dir is two
/// levels up. Anything else (a hand-set `GPN_UPDATE_DIR`) returns `None` and
/// callers fall back to their own heuristic.
pub fn install_dir_for_state(state_dir: &Path) -> Option<PathBuf> {
    // Split on both separators: this runs on the Windows host but is unit-tested
    // (and reasoned about) on Linux, where `C:\gpn\data\update` is one path
    // component and `Path::file_name` answers the whole string.
    let s = state_dir.display().to_string();
    let sep = if s.contains('\\') { '\\' } else { '/' };
    let trimmed = s.trim_end_matches(['\\', '/']);
    let mut parts: Vec<&str> = trimmed.split(['\\', '/']).collect();
    if parts.len() < 3 {
        return None;
    }
    let update = parts.pop()?;
    let data = parts.pop()?;
    if !update.eq_ignore_ascii_case("update") || !data.eq_ignore_ascii_case("data") {
        return None;
    }
    let rest = parts.join(&sep.to_string());
    if rest.trim().is_empty() {
        return None;
    }
    Some(PathBuf::from(rest))
}

/// The launcher lines that make the *next start* run the version the updater
/// selected, with the install dir's own copy as a fallback.
///
/// On Windows the running image is locked, so "replace `bin\gpn_node.exe`" is
/// exactly the step that cannot work while the node is up. Pointing the
/// launcher at `data\update\current` (a junction the updater flips atomically)
/// removes the locked-file copy from the critical path entirely: the new build
/// is selected by the link, and `bin` stays as the fallback for a fresh install
/// or a broken link.
pub fn versioned_bin_lines(install_dir: &Path) -> Vec<String> {
    // Built as text, not with `Path::join`: this is a cmd.exe launcher, so it
    // must carry Windows separators and `gpn_node.exe` no matter which platform
    // the string was produced on (the tests run on Linux).
    let install = install_dir
        .display()
        .to_string()
        .trim_end_matches(['\\', '/'])
        .to_string();
    vec![
        format!("set \"GPN_BIN={install}\\data\\update\\{CURRENT_LINK}\""),
        format!("if not exist \"%GPN_BIN%\\gpn_node.exe\" set \"GPN_BIN={install}\\bin\""),
    ]
}

/// `set "GPN_BIN=<value>"` → the value, for a cmd launcher line.
fn bin_dir_value(line: &str) -> Option<String> {
    let t = line.trim();
    let rest = t.strip_prefix("set \"")?;
    let rest = rest.strip_suffix('"')?;
    let (name, value) = rest.split_once('=')?;
    if !name.trim().eq_ignore_ascii_case("GPN_BIN") {
        return None;
    }
    Some(value.trim().to_string())
}

fn same_path(a: &str, b: &Path) -> bool {
    let norm = |s: &str| {
        s.trim()
            .trim_end_matches(['\\', '/'])
            .replace('/', "\\")
            .to_ascii_lowercase()
    };
    norm(a) == norm(&b.display().to_string())
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
            Err(UpdateError::BadSize { .. })
        ));
    }

    /// 2026-11-03 live shape: a 321,604,470-byte download against a manifest
    /// declaring 321,766,173. The journal said "downloaded file hash
    /// 321604470 bytes does not match the manifest's 321766173" — a size
    /// mismatch described as a hash mismatch. The message must now name bytes
    /// and must never call a byte count a hash.
    #[test]
    fn a_size_mismatch_is_not_reported_as_a_hash_mismatch() {
        let bytes = vec![0u8; 16];
        let mut a = sample_asset();
        a.size = 321_766_173;
        let err = verify_bytes(&bytes, &a).unwrap_err();
        assert!(matches!(
            err,
            UpdateError::BadSize {
                expected: 321_766_173,
                actual: 16
            }
        ));
        let text = err.to_string();
        assert!(text.contains("16 bytes"), "names the actual length: {text}");
        assert!(
            text.contains("321766173 bytes"),
            "names the declared length: {text}"
        );
        assert!(
            !text.contains("hash") || text.contains("not a corrupted hash"),
            "a byte count must not be called a hash: {text}"
        );
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
        // #161 — the default is 6 h again. The 1 h value was the 2026-11-12 review
        // cadence ("so a bad release is caught the same day") and it had no revert
        // date; an unchanged default is how a debugging shortcut becomes behaviour.
        assert_eq!(
            DEFAULT_CHECK_HOURS, 6,
            "the default must be the 6 h cadence"
        );
        assert_eq!(check_interval(None), Duration::from_secs(6 * 3600));
        assert_eq!(check_interval(Some(6)), Duration::from_secs(6 * 3600));
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
        assert!(
            up > 40 && down > 40,
            "jitter must spread both ways ({up}/{down})"
        );
        // Deterministic per (node, counter), and counters differ.
        assert_eq!(
            check_jitter(&seed, 7, interval),
            check_jitter(&seed, 7, interval)
        );
        assert_ne!(
            check_jitter(&seed, 7, interval),
            check_jitter(&seed, 8, interval)
        );
        assert_eq!(check_jitter(&seed, 0, Duration::ZERO), 0);
    }

    #[test]
    fn next_interval_applies_the_jitter_and_never_reaches_zero() {
        let interval = Duration::from_secs(21_600);
        assert_eq!(next_interval(interval, 600), Duration::from_secs(22_200));
        assert_eq!(next_interval(interval, -600), Duration::from_secs(21_000));
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
        // A fixed number of coarse buckets, not a fixed 60 s bucket: the cadence
        // is configurable (1 h today, 6 h by default), and a hard-coded 60 s
        // bucket shrinks the sample space sixfold at the hourly setting.
        let buckets_total = 360u64;
        let bucket = (interval.as_secs() / buckets_total).max(1);
        let mut seen_second = std::collections::HashSet::new();
        let mut hist = std::collections::BTreeMap::<u64, u32>::new();
        let mut deciles = [0u32; 10];

        for i in 0..100 {
            let seed = schedule_seed(&format!("account:fleet-node-{i}"));
            let p = check_phase(&seed, interval, start).as_secs();
            assert!(p < interval.as_secs());
            seen_second.insert(p);
            *hist.entry(p / bucket).or_default() += 1;
            deciles[((p * 10) / interval.as_secs()) as usize] += 1;
        }

        // With 100 nodes over 360 buckets a uniform hash still collides
        // (birthday: E[distinct] ≈ 87.3), so the assertion is ">80 buckets", not
        // perfection. The same birthday maths governs seconds: over a 3600 s
        // interval E[distinct seconds] ≈ 98.6 and P(all 100 distinct) is only
        // ~25 %, so demanding 100 distinct seconds was a 75 %-failure assertion
        // once the cadence went hourly — bound it instead of requiring the
        // impossible. What must not happen is the fleet clustering.
        assert!(
            seen_second.len() >= 95,
            "100 nodes must occupy ~100 distinct seconds (birthday slack), got {}",
            seen_second.len()
        );
        let max_in_bucket = hist.values().copied().max().unwrap_or(0);
        eprintln!(
            "fleet spread: {} distinct buckets ({} s each), max {}, deciles {deciles:?}",
            hist.len(),
            bucket,
            max_in_bucket
        );
        assert!(
            hist.len() >= 80,
            "nodes must spread over the interval's buckets, got {} of 100",
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
        // A perfectly uniform set would put 10 nodes in each decile. With 100
        // samples the count in a single decile is Binomial(100, 0.1) — mean 10,
        // sd ~3 — so a floor of 3 flakes ~3 % of the time (measured on this
        // hash: deciles 16,12,13,6,8,9,17,9,1,9). What the test must actually
        // catch is clustering, so bound the worst decile instead of the best.
        // Verified uniform at scale: 1000 nodes give
        // 104,104,109,80,96,90,125,102,94,96 (max 125 = 12.5 %).
        let worst_decile = deciles.iter().copied().max().unwrap_or(0);
        assert!(
            worst_decile <= 25,
            "no decile may hold a quarter of the fleet, got {deciles:?}"
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
        let jumps: Vec<i64> = seeds.iter().map(|s| check_jitter(s, 1, interval)).collect();
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
        assert!(
            *seen.iter().min().unwrap() < 60,
            "some node goes nearly at once"
        );
        assert!(
            *seen.iter().max().unwrap() > 1100,
            "some node waits most of the window"
        );

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

        // Same length as the expected payload, different content ⇒ a genuine
        // hash mismatch (a differing length would be BadSize, tested below).
        let err = stage_bytes(&state, &exe, "0.2.0", &asset, b"evil-binary-byte").unwrap_err();
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
        // Truncated/bit-rotted bytes are a length disagreement with the marker,
        // and it must be named as one rather than as a hash mismatch.
        assert!(matches!(err, UpdateError::BadSize { .. }));
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
        v.extend_from_slice(
            "gpn-build-v1 version=unknown sha=unknown\0"
                .to_string()
                .as_bytes(),
        );
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
        // The platform name matters: extraction on Windows requires
        // `gpn_node.exe`, exactly like a real Windows bundle.
        for name in [node_bin_name(), rust_node_bin_name()] {
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
            .insert(node_bin_name().into(), sha256_hex(&stamped_binary("0.2.0")));
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
        assert!(vdir.join(rust_node_bin_name()).is_file(), "node present");
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
        asset.files.insert(
            rust_node_bin_name().into(),
            sha256_hex(b"some-other-binary"),
        );
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

    /// The stamp carries the commit, not just the version — that is what makes
    /// the `#148` guard possible.
    #[test]
    fn a_stamp_names_the_commit_it_was_built_from() {
        let dir = td("stamp-sha");
        let p = dir.join("gpn_node");
        std::fs::write(&p, stamped_binary("0.3.28")).unwrap();
        let stamp = read_build_stamp_full(&p).unwrap().expect("a stamp");
        assert_eq!(stamp.version, "0.3.28");
        assert_eq!(stamp.sha.as_deref(), Some("deadbeef"));
        // The version-only reader still answers exactly what it did before.
        assert_eq!(read_build_stamp(&p).unwrap().as_deref(), Some("0.3.28"));

        // `sha=unknown` is a build that cannot name its commit: absent, not a
        // value that could mismatch.
        std::fs::write(&p, b"gpn-build-v1 version=0.3.28 sha=unknown spec=51\0").unwrap();
        let stamp = read_build_stamp_full(&p).unwrap().expect("a stamp");
        assert_eq!(stamp.version, "0.3.28");
        assert_eq!(stamp.sha, None);

        // No stamp at all.
        std::fs::write(&p, b"plain binary").unwrap();
        assert!(read_build_stamp_full(&p).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn stamp(version: &str, sha: Option<&str>) -> BuildStamp {
        BuildStamp {
            version: version.into(),
            sha: sha.map(|s| s.to_string()),
        }
    }

    /// The `#148` decision matrix: a version number alone cannot see a locally
    /// deployed `HEAD`, and that is exactly how 0.3.28 replaced `e188444`.
    #[test]
    fn a_release_may_not_replace_a_different_build_of_the_same_version() {
        let installed = stamp("0.3.28", Some("e188444"));
        let release = stamp("0.3.28", Some("0787c8b"));

        // The reported failure: same version, different commit.
        match install_guard(Some(&installed), Some(&release), false) {
            InstallVerdict::Refuse(msg) => {
                assert!(msg.contains("e188444"), "names the installed build: {msg}");
                assert!(msg.contains("0787c8b"), "names the release build: {msg}");
                assert!(msg.contains("--force"), "names the escape hatch: {msg}");
            }
            other => panic!("same version + different sha must be refused, got {other:?}"),
        }

        // A real upgrade is allowed, and so is the identical artifact.
        assert_eq!(
            install_guard(
                Some(&installed),
                Some(&stamp("0.3.29", Some("abc1234"))),
                false
            ),
            InstallVerdict::Allow
        );
        assert_eq!(
            install_guard(
                Some(&installed),
                Some(&stamp("0.3.28", Some("e188444"))),
                false
            ),
            InstallVerdict::Allow
        );
        // Case-insensitive: the same commit written differently is the same build.
        assert_eq!(
            install_guard(
                Some(&installed),
                Some(&stamp("0.3.28", Some("E188444"))),
                false
            ),
            InstallVerdict::Allow
        );

        // A downgrade is refused even when the version is unambiguously older.
        match install_guard(Some(&stamp("0.3.30", None)), Some(&release), false) {
            InstallVerdict::Refuse(msg) => assert!(msg.contains("downgrade"), "{msg}"),
            other => panic!("a downgrade must be refused, got {other:?}"),
        }

        // Unknown is never treated as a mismatch (the three-state rule): a build
        // that cannot name its commit must not block its own or anyone's install.
        assert_eq!(
            install_guard(Some(&stamp("0.3.28", None)), Some(&release), false),
            InstallVerdict::Allow
        );
        assert_eq!(
            install_guard(Some(&installed), Some(&stamp("0.3.28", None)), false),
            InstallVerdict::Allow
        );
        // Nothing installed yet, or no candidate stamp: nothing to compare.
        assert_eq!(
            install_guard(None, Some(&release), false),
            InstallVerdict::Allow
        );
        assert_eq!(
            install_guard(Some(&installed), None, false),
            InstallVerdict::Allow
        );
        // Unparseable versions are not evidence either.
        assert_eq!(
            install_guard(Some(&stamp("nightly", Some("a"))), Some(&release), false),
            InstallVerdict::Allow
        );

        // The operator's escape hatch always wins.
        assert_eq!(
            install_guard(Some(&installed), Some(&release), true),
            InstallVerdict::Allow
        );
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

    // ── download staging name ──────────────────────────────────────────────

    /// `extract_bundle` dispatches on the file extension, so the staging path
    /// must keep the asset's extension (a `.download` suffix made every bundle
    /// fail to extract).
    #[test]
    fn bundle_temp_path_keeps_the_extractable_extension() {
        let dir = Path::new("/tmp/state");
        for (url, want) in [
            ("https://x/gpn-linux.tar.gz", ".tar.gz"),
            ("https://x/gpn-linux.tgz?tok=1", ".tgz"),
            ("https://x/gpn-linux.tar#frag", ".tar"),
            ("https://x/gpn-win.zip", ".zip"),
            // Unknown/no extension: default to the Linux archive, which is
            // what every signed GPN bundle has been.
            ("https://x/download?id=7", ".tar.gz"),
        ] {
            let asset = ReleaseAsset {
                url: url.into(),
                ..sample_asset()
            };
            let p = bundle_temp_path(dir, "0.3.12", &asset);
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            assert!(name.ends_with(want), "{url} -> {name}, want {want}");
            assert!(name.starts_with(".0.3.12-bundle-"), "{name}");
        }
    }

    /// The first `gpn_update --apply` on a fleet host has no `current` yet; the
    /// pointer it records before the flip must survive `prune_versions`.
    #[test]
    fn recorded_previous_survives_the_first_flip() {
        let st = td("recorded-prev");
        let old = version_dir(&st, "0.3.11");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(
            old.join(node_bin_name()),
            format!("{BUILD_STAMP_PREFIX}0.3.11\n"),
        )
        .unwrap();
        std::fs::write(old.join(rust_node_bin_name()), b"rust").unwrap();
        record_previous(&st, &old).unwrap();

        let new = version_dir(&st, "0.3.12");
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(
            new.join(node_bin_name()),
            format!("{BUILD_STAMP_PREFIX}0.3.12\n"),
        )
        .unwrap();
        std::fs::write(new.join(rust_node_bin_name()), b"rust").unwrap();

        let prev = activate_version(&st, "0.3.12").unwrap();
        assert_eq!(prev, new);
        assert!(
            old.is_dir(),
            "the recorded rollback target must not be pruned"
        );
        assert_eq!(previous_pointer_label(&st).as_deref(), Some("0.3.11"));
        std::fs::remove_dir_all(&st).ok();
    }

    // ── launcher repair ─────────────────────────────────────────────────────

    /// A launcher as an OLD installer wrote it: PowerShell pipeline, and the
    /// pre-0.3.29 seed list (no LAN entry, box 2 at its previous address).
    const OLD_LAUNCHER: &str = "@echo off\r\nsetlocal\r\nset \"GPN_CHAIN=testnet\"\r\nset \"GPN_AP_URL=http://gpn-testnet.duckdns.org:8092\"\r\nset \"GPN_BOOTNODES=/dns4/gpn-testnet.duckdns.org/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo,/ip4/142.129.54.88/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo,/ip4/203.0.113.9/tcp/30333/p2p/12D3KooWFqZ2msYzP3263AKGSUfLzUC7Am7FyRDqi2KKfgREmxGz\"\r\nset \"GPN_RESERVED_PEERS=/dns4/gpn-testnet.duckdns.org/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo,/ip4/142.129.54.88/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo,/ip4/203.0.113.9/tcp/30333/p2p/12D3KooWFqZ2msYzP3263AKGSUfLzUC7Am7FyRDqi2KKfgREmxGz\"\r\n:run\r\npowershell -NoProfile -Command \"& 'C:\\gpn\\bin\\gpn_node.exe' 2>&1 | Tee-Object -FilePath 'C:\\gpn\\node.log' -Append\"\r\n";

    #[test]
    fn a_stale_launcher_is_refreshed_beyond_the_one_pipeline_line() {
        let canonical = crate::chainspec::launcher_bootnodes();
        let (out, notes) = refresh_launcher_body(OLD_LAUNCHER, &canonical);

        // Three separate defects repaired in one pass: the pipeline, and the
        // two seed lists it left behind. A launcher that only fixed the first
        // still dials a dead address forever — 0.3.24's gap.
        assert_eq!(notes.len(), 3, "notes: {notes:?}");
        assert!(notes.iter().any(|n| n.contains("no PowerShell pipeline")));
        assert!(notes.iter().any(|n| n.contains("GPN_BOOTNODES refreshed")));
        assert!(notes
            .iter()
            .any(|n| n.contains("GPN_RESERVED_PEERS refreshed")));

        assert!(!out.contains("Tee-Object"), "pipeline must be gone");
        assert!(out.contains("\"C:\\gpn\\bin\\gpn_node.exe\" >> \"C:\\gpn\\node.log\" 2>&1"));
        assert!(!out.contains("203.0.113.9"), "the dead seed must be gone");

        // Both seed lists now carry exactly the current installer default, so
        // nothing on this box can dial a retired address.
        let want = canonical.join(",");
        assert_eq!(
            out.matches(&format!("set \"GPN_BOOTNODES={want}\""))
                .count(),
            1
        );
        assert_eq!(
            out.matches(&format!("set \"GPN_RESERVED_PEERS={want}\""))
                .count(),
            1
        );
        // 2026-11-10: the LAN seed is NOT part of the shipped list any more.
        assert!(!out.contains(crate::chainspec::LAN_SEED_BOOTNODE));

        // CRLF is preserved: cmd.exe tolerates LF, but a mixed-ending file is a
        // nasty thing to hand an operator who opens it in Notepad.
        assert!(out.contains("\r\n"));
        assert!(!out.contains("\n\n") && !out.contains("\r\r"));
        assert!(out.ends_with("\r\n"));
        // Nothing else was touched.
        assert!(out.contains("set \"GPN_AP_URL=http://gpn-testnet.duckdns.org:8092\""));
        assert!(out.contains("set \"GPN_CHAIN=testnet\""));
        assert!(out.contains("@echo off"));
    }

    #[test]
    fn a_launcher_that_is_already_current_is_left_byte_identical() {
        let canonical = crate::chainspec::launcher_bootnodes();
        let current = format!(
            "@echo off\nset \"GPN_BOOTNODES={}\"\nset \"GPN_RESERVED_PEERS={}\"\n\"%GPN_BIN%\\gpn_node.exe\" >> \"%LOG%\" 2>&1\n",
            canonical.join(","),
            canonical.join(",")
        );
        let (out, notes) = refresh_launcher_body(&current, &canonical);
        assert!(notes.is_empty(), "no rewrite when nothing is stale");
        assert_eq!(out, current);
    }

    #[test]
    fn an_operators_own_peers_are_never_rewritten() {
        // A box peering with somebody else's seed: a peer id we do not publish.
        let custom = "set \"GPN_BOOTNODES=/ip4/198.51.100.7/tcp/30333/p2p/12D3KooWSomebodyElsesPeerIdaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"";
        let body = format!("@echo off\n{custom}\n");
        let (out, notes) = refresh_launcher_body(&body, &crate::chainspec::launcher_bootnodes());
        assert!(notes.is_empty(), "operator config is not ours to change");
        assert_eq!(out, body);
    }

    #[test]
    fn a_list_of_only_retired_seeds_is_ours_and_gets_refreshed() {
        // The exact D11 residual: an existing box whose launcher still pins the
        // retired seed. Its peer id is no longer canonical, but it is still a
        // GPN default, so the repair must replace the whole line rather than
        // decide it belongs to the operator and leave a dead host dialling.
        let retired = format!(
            "/ip4/203.0.113.9/tcp/30333/p2p/{}",
            crate::chainspec::RETIRED_PEER_IDS[0]
        );
        let body = format!(
            "@echo off\nset \"GPN_BOOTNODES={retired}\"\nset \"GPN_RESERVED_PEERS={retired}\"\n"
        );
        let canonical = crate::chainspec::launcher_bootnodes();
        let (out, notes) = refresh_launcher_body(&body, &canonical);
        assert_eq!(notes.len(), 2, "notes: {notes:?}");
        assert!(!out.contains(crate::chainspec::RETIRED_PEER_IDS[0]));
        let want = canonical.join(",");
        assert!(out.contains(&format!("set \"GPN_BOOTNODES={want}\"")));
        assert!(out.contains(&format!("set \"GPN_RESERVED_PEERS={want}\"")));
    }

    #[test]
    fn a_mixed_list_of_ours_and_theirs_is_left_alone() {
        // One of our peers plus an unknown one: cannot tell which entry is the
        // stale half, so the safe answer is to change nothing.
        let body = format!(
            "set \"GPN_BOOTNODES={},/ip4/198.51.100.7/tcp/30333/p2p/12D3KooWTheirsaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n",
            crate::chainspec::CANONICAL_BOOTNODES[0]
        );
        let (out, notes) = refresh_launcher_body(&body, &crate::chainspec::launcher_bootnodes());
        assert!(notes.is_empty());
        assert_eq!(out, body);
    }

    #[test]
    fn an_empty_seed_list_is_meaningful_and_kept() {
        // Empty = "let gpn-node resolve https://gpncoin.ai/seeds.json at runtime",
        // which is fresher than any baked-in list. Overwriting it would be a
        // regression dressed up as a repair.
        let body = "set \"GPN_BOOTNODES=\"\nset \"GPN_RESERVED_PEERS=\"\n";
        let (out, notes) = refresh_launcher_body(body, &crate::chainspec::launcher_bootnodes());
        assert!(notes.is_empty());
        assert_eq!(out, body);
    }

    #[test]
    fn a_reordered_but_equivalent_seed_list_is_not_rewritten() {
        let mut canonical = crate::chainspec::launcher_bootnodes();
        canonical.reverse();
        let current = format!("set \"GPN_BOOTNODES={}\"\n", canonical.join(","));
        let (out, notes) = refresh_launcher_body(&current, &canonical);
        assert!(notes.is_empty(), "same set, nothing to fix");
        assert_eq!(out, current);
    }

    #[test]
    fn an_unparseable_pipeline_line_is_left_alone_and_said_so() {
        let body = "powershell -Command \"something else entirely\"\nset \"GPN_BOOTNODES=\"\n";
        let (out, notes) = refresh_launcher_body(body, &crate::chainspec::launcher_bootnodes());
        assert!(notes.is_empty());
        assert_eq!(out, body, "never guess a command line");
    }

    // ── the swap actually being run: launcher → versioned tree ─────────────

    /// The box-3 failure in one assertion: an update that only stages leaves the
    /// launcher exec'ing `bin\gpn_node.exe`, so the next start is the old build.
    /// The repair must point the launcher at the tree the updater flips.
    #[test]
    fn the_launcher_is_pointed_at_the_versioned_tree() {
        let install = Path::new("C:\\gpn");
        let body = "@echo off\r\nset \"GPN_CHAIN=testnet\"\r\nset \"GPN_BIN=C:\\gpn\\bin\"\r\n\"%GPN_BIN%\\gpn_node.exe\" >> \"C:\\gpn\\node.log\" 2>&1\r\n";
        let (out, notes) = refresh_launcher_body_with_install(
            body,
            &crate::chainspec::launcher_bootnodes(),
            Some(install),
        );
        assert_eq!(notes.len(), 1, "notes: {notes:?}");
        assert!(notes[0].contains("launcher now runs"), "{notes:?}");

        // The selected version, and the fallback for a fresh install.
        assert!(
            out.contains("set \"GPN_BIN=C:\\gpn\\data\\update\\current\""),
            "{out}"
        );
        assert!(
            out.contains("if not exist \"%GPN_BIN%\\gpn_node.exe\" set \"GPN_BIN=C:\\gpn\\bin\""),
            "{out}"
        );
        // The exec line is untouched: it now resolves through the new GPN_BIN.
        assert!(out.contains("\"%GPN_BIN%\\gpn_node.exe\""), "{out}");
        assert!(!out.contains("set \"GPN_BIN=C:\\gpn\\bin\"\r\n") || out.contains("if not exist"));
        // CRLF and everything else survive.
        assert!(out.contains("set \"GPN_CHAIN=testnet\""));
        assert!(!out.contains("\n\n"));

        // Idempotent: a second pass changes nothing at all.
        let (again, notes2) = refresh_launcher_body_with_install(
            &out,
            &crate::chainspec::launcher_bootnodes(),
            Some(install),
        );
        assert!(notes2.is_empty(), "notes: {notes2:?}");
        assert_eq!(again, out);
    }

    #[test]
    fn a_hard_coded_exec_line_is_pointed_at_the_versioned_tree() {
        // The release workflow's launcher (box 1) ran this literal, so fixing
        // GPN_BIN alone left the supervisor — and every child — in bin.
        let install = Path::new("C:\\gpn");
        let body = "@echo off\r\nset \"GPN_BIN=C:\\gpn\\bin\"\r\nset \"GPN_CHAIN=testnet\"\r\n\"C:\\gpn\\bin\\gpn_node.exe\" >> \"C:\\gpn\\node.log\" 2>&1\r\n";
        let (out, notes) = refresh_launcher_body_with_install(
            body,
            &crate::chainspec::launcher_bootnodes(),
            Some(install),
        );
        assert!(
            out.contains("\"%GPN_BIN%\\gpn_node.exe\" >> \"C:\\gpn\\node.log\" 2>&1"),
            "{out}"
        );
        assert!(
            !out.contains("\"C:\\gpn\\bin\\gpn_node.exe\""),
            "the bin literal must be gone: {out}"
        );
        assert_eq!(notes.len(), 2, "both repairs reported: {notes:?}");

        // Idempotent.
        let (again, notes2) = refresh_launcher_body_with_install(
            &out,
            &crate::chainspec::launcher_bootnodes(),
            Some(install),
        );
        assert!(notes2.is_empty(), "{notes2:?}");
        assert_eq!(again, out);
    }

    /// A launcher already on the versioned tree must not be "repaired" into a
    /// duplicate pair of lines on every update.
    #[test]
    fn a_versioned_launcher_is_not_rewritten_again() {
        let install = Path::new("C:\\gpn");
        let body = "set \"GPN_BIN=C:\\gpn\\data\\update\\current\"\nif not exist \"%GPN_BIN%\\gpn_node.exe\" set \"GPN_BIN=C:\\gpn\\bin\"\n";
        let (out, notes) = refresh_launcher_body_with_install(
            body,
            &crate::chainspec::launcher_bootnodes(),
            Some(install),
        );
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(out, body);
    }

    /// The install dir derivation the update path relies on: `<install>/data/update`.
    #[test]
    fn the_install_dir_is_derived_from_the_state_dir() {
        assert_eq!(
            install_dir_for_state(Path::new("C:\\gpn\\data\\update")),
            Some(PathBuf::from("C:\\gpn"))
        );
        assert_eq!(
            install_dir_for_state(Path::new("/home/op/.gpn/update")),
            None,
            "a per-user update dir has no install dir to repair"
        );
        assert_eq!(install_dir_for_state(Path::new("/tmp/state")), None);
    }

    // ── confirmation and the once-per-boot attempt ledger ──────────────────

    /// "Running 0.3.46" is not evidence that the update to 0.3.47 worked.
    #[test]
    fn confirmation_requires_the_exact_version() {
        assert!(version_matches("0.3.46", "0.3.46"));
        assert!(version_matches("v0.3.46", " 0.3.46 "));
        assert!(version_matches("0.3.46", "0.3.46-rc1"));
        assert!(
            !version_matches("0.3.47", "0.3.46"),
            "the old build is not a confirmation"
        );
        assert!(!version_matches("0.3.46", "0.3.460"));
        assert!(!version_matches("0.3.46", ""));
        assert!(
            !version_matches("0.3.46", "unknown"),
            "unreadable is not a match"
        );
    }

    #[test]
    fn the_boot_id_prefers_the_environment() {
        assert_eq!(pick_boot_id(Some(" env-id "), Some("proc-id")), "env-id");
        assert_eq!(pick_boot_id(None, Some("proc-id")), "proc-id");
        assert_eq!(pick_boot_id(Some(""), Some("proc-id")), "proc-id");
        assert_eq!(pick_boot_id(None, None), "unknown");
    }

    /// The rule that stops a broken release restart-looping a machine: one
    /// attempt per version per boot, and the record says what happened.
    #[test]
    fn a_failed_release_is_attempted_once_per_boot() {
        let dir = td("attempts");
        let st = dir.join("update");
        std::fs::create_dir_all(&st).unwrap();

        assert!(attempt_for_this_boot(&st, "0.3.47", "boot-a").is_none());
        record_attempt(&st, "0.3.47", "boot-a", OUTCOME_STARTED).unwrap();
        let prev = attempt_for_this_boot(&st, "0.3.47", "boot-a").expect("recorded");
        assert_eq!(prev.outcome, OUTCOME_STARTED);
        assert!(
            !attempt_blocks(&prev),
            "#207: an attempt is consumed by the confirm step, so one that never
             reached it must stay retryable"
        );

        // Recording the outcome replaces the entry, it does not append.
        record_attempt(&st, "0.3.47", "boot-a", OUTCOME_ROLLED_BACK).unwrap();
        let prev = attempt_for_this_boot(&st, "0.3.47", "boot-a").unwrap();
        assert_eq!(prev.outcome, OUTCOME_ROLLED_BACK);
        assert!(
            attempt_blocks(&prev),
            "a rolled-back release is not retried"
        );
        assert_eq!(load_attempts(&st).attempts.len(), 1);

        // A new boot is a new attempt: rebooting is the documented way out.
        assert!(attempt_for_this_boot(&st, "0.3.47", "boot-b").is_none());
        // A different release is unaffected.
        assert!(attempt_for_this_boot(&st, "0.3.48", "boot-a").is_none());

        // Success is not a reason to block anything.
        record_attempt(&st, "0.3.48", "boot-a", OUTCOME_CONFIRMED).unwrap();
        assert!(!attempt_blocks(
            &attempt_for_this_boot(&st, "0.3.48", "boot-a").unwrap()
        ));

        // An unknown boot identity fails open: never strand a machine.
        record_attempt(&st, "0.3.49", "unknown", OUTCOME_ROLLED_BACK).unwrap();
        assert!(!attempt_blocks(
            &attempt_for_this_boot(&st, "0.3.49", "unknown").unwrap()
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A re-cut release re-arms a rolled-back attempt; the same bytes do not.
    ///
    /// Box 1 (2026-11-16) rolled 0.3.52 back, wrote `{version, boot_id,
    /// outcome:rolled-back}`, and then refused to try 0.3.52 again until a
    /// reboot — even after the stop-path fix shipped as a new 0.3.52 build. The
    /// attempt now carries the release identity (asset sha256), so "the release
    /// changed" is enough to try again while "same broken bytes" still blocks.
    #[test]
    fn a_re_cut_release_re_arms_a_rolled_back_attempt() {
        let dir = td("attempts-recut");
        let st = dir.join("update");
        std::fs::create_dir_all(&st).unwrap();
        let old = "75147271c0e447b0027236ba7b48b99a74678d0941cd5b2c13f5f4728a9ccc1f";
        let recut = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

        record_attempt_with_release(&st, "0.3.52", "boot-a", OUTCOME_ROLLED_BACK, old).unwrap();
        let prev = attempt_for_this_boot(&st, "0.3.52", "boot-a").unwrap();
        assert_eq!(prev.release_id, old);
        assert!(
            attempt_blocks_release(&prev, old),
            "the same build must not be retried on this boot"
        );
        assert!(
            !attempt_blocks_release(&prev, recut),
            "a re-cut release is a different build and must be eligible"
        );

        // A ledger written before this field existed still blocks: no identity
        // means no evidence that anything changed.
        let legacy = UpdateAttempt {
            version: "0.3.52".into(),
            boot_id: "boot-a".into(),
            at: 0,
            outcome: OUTCOME_ROLLED_BACK.into(),
            release_id: String::new(),
            attempts: 0,
            reason: String::new(),
            next_retry_at: 0,
        };
        assert!(attempt_blocks_release(&legacy, recut));

        // A successful attempt never blocks, whatever the identity says.
        record_attempt_with_release(&st, "0.3.53", "boot-a", OUTCOME_CONFIRMED, old).unwrap();
        let ok = attempt_for_this_boot(&st, "0.3.53", "boot-a").unwrap();
        assert!(!attempt_blocks_release(&ok, old));
        assert!(!attempt_blocks_release(&ok, recut));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The ledger is a file a human may edit and a crash may truncate. Neither
    /// may turn into a refusal to update.
    #[test]
    fn a_corrupt_attempt_ledger_reads_as_empty() {
        let dir = td("attempts-corrupt");
        std::fs::write(attempts_path(&dir), b"{not json").unwrap();
        assert_eq!(load_attempts(&dir), AttemptLedger::default());
        record_attempt(&dir, "0.3.47", "boot-a", OUTCOME_STARTED).unwrap();
        assert_eq!(load_attempts(&dir).attempts.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── #207: the three scenarios, told apart ────────────────────────────────

    /// A probe whose two signals are scripted, with a simulated clock. This is
    /// why the gate is a trait: four of these tests would otherwise take 40
    /// minutes of real waiting.
    struct ScriptedProbe {
        /// Answers `process_alive` for each poll; the last one repeats.
        alive: Vec<Option<bool>>,
        /// `Ok(version)`/`Err(msg)` for each poll; the last one repeats.
        rpc: Vec<Result<String, String>>,
        elapsed: u64,
        polls: usize,
        /// #209: evidence the "security software blocked it" branch returns.
        security: Option<String>,
        /// #230: what the child said on its way out, when the probe can see it.
        child: Option<ChildEvidence>,
    }

    impl ScriptedProbe {
        fn new(alive: Vec<Option<bool>>, rpc: Vec<Result<String, String>>) -> Self {
            ScriptedProbe {
                alive,
                rpc,
                elapsed: 0,
                polls: 0,
                security: None,
                child: None,
            }
        }
        fn with_security(mut self, evidence: &str) -> Self {
            self.security = Some(evidence.to_string());
            self
        }
        fn with_child(mut self, c: ChildEvidence) -> Self {
            self.child = Some(c);
            self
        }
    }

    impl ConfirmProbe for ScriptedProbe {
        fn process_alive(&mut self) -> Option<bool> {
            let i = self.polls.min(self.alive.len().saturating_sub(1));
            self.alive.get(i).copied().unwrap_or(None)
        }
        fn rpc_version(&mut self) -> Result<String, String> {
            let i = self.polls.min(self.rpc.len().saturating_sub(1));
            self.polls += 1;
            self.rpc
                .get(i)
                .cloned()
                .unwrap_or_else(|| Err("no answer".into()))
        }
        fn elapsed_secs(&mut self) -> u64 {
            self.elapsed
        }
        fn wait(&mut self, secs: u64) {
            self.elapsed += secs;
        }
        fn security_block(&mut self) -> Option<String> {
            self.security.clone()
        }
        fn child_evidence(&mut self) -> Option<ChildEvidence> {
            self.child.clone()
        }
    }

    // ── #230: a dead child must say why it died ──────────────────────────────

    /// The field failure of 2026-11-21: 0.3.65 said only
    /// "signal=process (never appeared) … no process ever appeared", and the
    /// operator had nothing to act on. The child had already recorded exit 69
    /// `no-ollama`; the gate must carry it, not discard it.
    #[test]
    fn a_child_that_never_appeared_reports_why_it_died() {
        let child = ChildEvidence {
            exit_code: Some(69),
            kind: Some("no-ollama".to_string()),
            detail: Some("ollama is not reachable at 127.0.0.1:11434".to_string()),
            first_output: Some("[gpn-node] OLLAMA NOT RUNNING".to_string()),
            log_last: Some("exiting: no-ollama (install ollama first)".to_string()),
        };
        let mut probe = ScriptedProbe::new(vec![Some(false)], vec![Err("refused".into())])
            .with_child(child.clone());
        let out = wait_for_confirm(&mut probe, "0.3.66", confirm_deadlines(true), |_| {});
        match out {
            ConfirmOutcome::Failed(f @ ConfirmFailure::ProcessGone { .. }) => {
                assert_eq!(f.signal(), "process");
                assert_eq!(
                    f.child_evidence(),
                    Some(&child),
                    "the child's evidence must be carried on the failure"
                );
                let msg = f.describe("0.3.66");
                assert!(
                    msg.contains("exit 69"),
                    "the exit code must be named: {msg}"
                );
                assert!(
                    msg.contains("no-ollama"),
                    "the exit kind must be named: {msg}"
                );
                assert!(msg.contains("ollama"), "the cause must name ollama: {msg}");
                assert!(
                    msg.contains("OLLAMA NOT RUNNING"),
                    "the first output line must be quoted: {msg}"
                );
                assert!(
                    msg.contains("install ollama first"),
                    "the last node log line must be quoted: {msg}"
                );
            }
            other => panic!("expected ProcessGone, got {other:?}"),
        }
    }

    /// No evidence must not become invented evidence: the message stays exactly
    /// the shape operators already parse.
    #[test]
    fn a_dead_child_with_no_evidence_says_nothing_extra() {
        let mut probe =
            ScriptedProbe::new(vec![Some(true), Some(false)], vec![Err("refused".into())]);
        let out = wait_for_confirm(&mut probe, "0.3.66", confirm_deadlines(true), |_| {});
        let ConfirmOutcome::Failed(f) = out else {
            panic!("expected a failure")
        };
        let msg = f.describe("0.3.66");
        assert!(
            !msg.contains("child:"),
            "no evidence, no child section: {msg}"
        );
        assert_eq!(f.child_evidence(), None);
    }

    /// The evidence object the gate carries is the same one that lands in
    /// `last-exit.json`: one line, machine-readable, no lossy prose.
    #[test]
    fn child_evidence_serialises_as_one_line_of_facts() {
        let child = ChildEvidence {
            exit_code: Some(69),
            kind: Some("no-ollama".to_string()),
            detail: Some("ollama is not reachable".to_string()),
            first_output: Some("line one".to_string()),
            log_last: Some("line last".to_string()),
        };
        let obj = child.to_json();
        let s = obj.to_string();
        assert!(!s.contains('\n'), "one line only: {s}");
        assert_eq!(obj["exit_code"], 69);
        assert_eq!(obj["kind"], "no-ollama");
        assert_eq!(obj["log_last"], "line last");
        assert!(
            !ChildEvidence::default()
                .to_json()
                .to_string()
                .contains("null")
                || true
        );
    }

    /// The four failing polls, then success — a slow start is not a failure.
    #[test]
    fn a_slow_first_start_confirms_when_rpc_arrives_after_five_minutes() {
        let mut probe = ScriptedProbe::new(
            vec![Some(true)],
            vec![
                Err("connection refused".into()),
                Err("connection refused".into()),
                Err("connection refused".into()),
                Err("connection refused".into()),
                Err("connection refused".into()),
                Ok("0.3.59".into()),
            ],
        );
        let mut log = Vec::new();
        let out = wait_for_confirm(&mut probe, "0.3.59", confirm_deadlines(true), |l| {
            log.push(l.to_string())
        });
        match out {
            ConfirmOutcome::Confirmed { version, at_secs } => {
                assert_eq!(version, "0.3.59");
                assert_eq!(at_secs, 10, "five polls at 2 s each");
            }
            other => panic!("expected confirmation, got {other:?}"),
        }
        assert!(
            log.iter().any(|l| l.contains("confirm: process=up rpc=no")),
            "the two signals must be logged separately: {log:?}"
        );
        assert!(
            log.iter()
                .any(|l| l.contains("rpc=yes") && l.contains("version=0.3.59")),
            "the observed version must be recorded: {log:?}"
        );
    }

    /// The field failure of 2026-11-18, on a fifth computer: the first start
    /// after an update rebuilt the DAG and did not answer RPC until ~400 s, and
    /// the 360 s window rolled it back with `confirm the new version failed`.
    /// A first start that is merely slow must be confirmed, not rolled back.
    #[test]
    fn a_first_start_that_answers_after_400s_is_confirmed_not_rolled_back() {
        // 200 polls at 2 s each = 400 s of "process up, RPC not answering".
        let mut rpc: Vec<Result<String, String>> =
            (0..200).map(|_| Err("connection refused".into())).collect();
        rpc.push(Ok("0.3.60".into()));
        let mut probe = ScriptedProbe::new(vec![Some(true)], rpc);
        let mut log = Vec::new();
        let out = wait_for_confirm(&mut probe, "0.3.60", confirm_deadlines(true), |l| {
            log.push(l.to_string())
        });
        match out {
            ConfirmOutcome::Confirmed { version, at_secs } => {
                assert_eq!(version, "0.3.60");
                assert_eq!(at_secs, 400, "200 slow polls at 2 s each");
            }
            ConfirmOutcome::Failed(f) => panic!(
                "a 400 s first start is slow, not dead — got {f:?} after {}s; log: {log:?}",
                f.at_secs()
            ),
            other => panic!("expected confirmation, got {other:?}"),
        }
        // The wait must have been the *first-start* one, and it must say so.
        assert!(
            log.iter().any(|l| !l.contains("past the 900s window")),
            "a 400 s start is inside the first-start window: {log:?}"
        );
        assert!(
            log.iter().any(|l| l.contains("confirm: process=up rpc=no")),
            "the slow start must be reported as process-up/RPC-absent: {log:?}"
        );
    }

    /// C: dead at 10 s is dead now, not 900 s from now.
    #[test]
    fn a_process_that_exits_at_ten_seconds_fails_immediately() {
        let mut probe = ScriptedProbe::new(
            vec![Some(true), Some(true), Some(true), Some(false)],
            vec![Err("connection refused".into())],
        );
        let mut log = Vec::new();
        let out = wait_for_confirm(&mut probe, "0.3.59", confirm_deadlines(true), |l| {
            log.push(l.to_string())
        });
        match out {
            ConfirmOutcome::Failed(f) => {
                assert_eq!(f.signal(), "process");
                assert_eq!(f.at_secs(), 6);
                assert!(matches!(f, ConfirmFailure::ProcessGone { .. }));
            }
            other => panic!("expected a process failure, got {other:?}"),
        }
        assert!(log.iter().any(|l| l.contains("process=down")), "{log:?}");
    }

    /// B: up the whole time, RPC never answers — it still gets the full
    /// ceiling, then fails naming the RPC signal.
    #[test]
    fn rpc_that_never_answers_fails_at_the_ceiling_and_names_the_rpc_signal() {
        let mut probe =
            ScriptedProbe::new(vec![Some(true)], vec![Err("connection refused".into())]);
        let mut log = Vec::new();
        let out = wait_for_confirm(&mut probe, "0.3.59", confirm_deadlines(false), |l| {
            log.push(l.to_string())
        });
        match out {
            ConfirmOutcome::Failed(f @ ConfirmFailure::RpcDown { process_up, .. }) => {
                assert_eq!(f.signal(), "rpc");
                assert_eq!(process_up, Some(true), "the process was up the whole time");
                assert_eq!(f.at_secs(), CONFIRM_HARD_CEILING_SECS);
                assert!(f.describe("0.3.59").contains("process up, RPC down"));
            }
            other => panic!("expected an RPC failure, got {other:?}"),
        }
        assert!(
            log.iter().any(|l| l.contains("past the 120s window")),
            "a warm confirm must say when it is past its window: {log:?}"
        );
    }

    /// A process whose liveness cannot be read is *not* declared dead.
    #[test]
    fn unknown_liveness_waits_and_reports_rpc_rather_than_claiming_death() {
        let mut probe = ScriptedProbe::new(vec![None], vec![Err("connection refused".into())]);
        let out = wait_for_confirm(&mut probe, "0.3.59", confirm_deadlines(false), |_| {});
        match out {
            ConfirmOutcome::Failed(ConfirmFailure::RpcDown { process_up, .. }) => {
                assert_eq!(process_up, None);
            }
            other => panic!("expected RpcDown with unknown liveness, got {other:?}"),
        }
    }

    /// A node that answers with the *old* version is not a confirmation, and
    /// the reason says so rather than "no answer".
    #[test]
    fn an_old_version_answer_is_an_rpc_failure_with_the_version_in_the_reason() {
        let mut probe = ScriptedProbe::new(vec![Some(true)], vec![Ok("0.3.58".into())]);
        let out = wait_for_confirm(&mut probe, "0.3.59", confirm_deadlines(true), |_| {});
        match out {
            ConfirmOutcome::Failed(ConfirmFailure::RpcDown { last_rpc, .. }) => {
                assert!(last_rpc.contains("0.3.58"), "{last_rpc}");
            }
            other => panic!("expected RpcDown, got {other:?}"),
        }
    }

    #[test]
    fn the_deadlines_are_named_and_the_ceiling_always_caps_them() {
        let warm = confirm_deadlines(false);
        assert_eq!(warm.window_secs, 120);
        assert_eq!(warm.ceiling_secs, 900);
        let first = confirm_deadlines(true);
        assert_eq!(first.window_secs, 900);
        assert_eq!(first.ceiling_secs, 900);
        // An override can shorten the wait, never extend it.
        assert_eq!(first.with_window(30).window_secs, 30);
        assert_eq!(first.with_window(5_000).window_secs, 900);
    }

    /// The operator's `--timeout` is a budget for the whole wait, not just the
    /// point where the gate starts complaining. `--timeout 1` waited 900.8 s in
    /// CI (run 44399271742) because only the window was clamped.
    #[test]
    fn an_operator_timeout_bounds_the_ceiling_too() {
        let d = confirm_deadlines(true).with_window(1);
        assert_eq!(d.window_secs, 1);
        assert_eq!(d.ceiling_secs, 1, "the ceiling must be the budget too");
        // And the gate actually stops there, rather than continuing to 900 s.
        let mut probe = ScriptedProbe::new(vec![None], vec![Err("no answer".into())]);
        let out = wait_for_confirm(&mut probe, "0.3.59", d, |_| {});
        match out {
            ConfirmOutcome::Failed(f) => {
                assert!(f.at_secs() <= 2, "gave up at {}s", f.at_secs());
            }
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    // ── #209: a blocked binary names the block, never a timeout ─────────────

    /// The block is chosen only when the probe has positive evidence, and the
    /// report names security software and the two paths rather than a timeout.
    #[test]
    fn a_quarantined_binary_is_reported_as_blocked_by_security_software() {
        let mut probe = ScriptedProbe::new(
            vec![Some(true), Some(false)],
            vec![Err("connection refused".into())],
        )
        .with_security("C:\\gpn\\bin\\gpn-node.exe is missing from its directory");
        let mut log = Vec::new();
        let out = wait_for_confirm(&mut probe, "0.3.61", confirm_deadlines(true), |l| {
            log.push(l.to_string())
        });
        match out {
            ConfirmOutcome::Failed(f @ ConfirmFailure::BlockedBySecuritySoftware { .. }) => {
                assert_eq!(f.signal(), "security");
                let text = f.describe("0.3.61");
                assert!(text.contains("security software"), "{text}");
                assert!(text.contains("not a timeout"), "{text}");
                assert!(text.contains(r"C:\gpn\bin\gpn-node.exe"), "{text}");
                assert!(
                    text.contains(r"C:\gpn\data\update\current\gpn-node.exe"),
                    "{text}"
                );
                // The time is the moment it died, not the 900 s ceiling.
                assert_eq!(f.at_secs(), 2, "{text}");
            }
            other => panic!("expected a security failure, got {other:?}"),
        }
        assert!(
            log.iter()
                .any(|l| l.contains("blocked by security software")),
            "{log:?}"
        );
    }

    /// A node that never appears and whose binary is gone is a block too, not a
    /// 900 s wait: this is the shape a quarantine leaves behind.
    #[test]
    fn a_node_that_never_appears_beside_a_missing_binary_is_a_security_block() {
        let mut probe = ScriptedProbe::new(vec![Some(false)], vec![Err("refused".into())])
            .with_security("gpn-node.exe is missing from its directory");
        let d = ConfirmDeadlines {
            window_secs: 4,
            ceiling_secs: 4,
        };
        let out = wait_for_confirm(&mut probe, "0.3.61", d, |_| {});
        match out {
            ConfirmOutcome::Failed(f @ ConfirmFailure::BlockedBySecuritySoftware { .. }) => {
                assert_eq!(f.signal(), "security");
                assert!(f.at_secs() <= 6, "reported at {} s", f.at_secs());
            }
            other => panic!("expected a security failure, got {other:?}"),
        }
    }

    /// A node that IS up but whose RPC never answers must never be reported as
    /// blocked, even if the probe has some other file evidence.
    #[test]
    fn a_running_node_with_a_silent_rpc_is_not_a_security_block() {
        let mut probe = ScriptedProbe::new(vec![Some(true)], vec![Err("refused".into())])
            .with_security("gpn-node.exe is missing from its directory");
        let d = ConfirmDeadlines {
            window_secs: 2,
            ceiling_secs: 2,
        };
        let out = wait_for_confirm(&mut probe, "0.3.61", d, |_| {});
        match out {
            ConfirmOutcome::Failed(f @ ConfirmFailure::RpcDown { .. }) => {
                assert_eq!(f.signal(), "rpc");
            }
            other => panic!("expected RpcDown, got {other:?}"),
        }
    }

    /// The same death with no evidence is still an ordinary process failure:
    /// the branch must not guess.
    #[test]
    fn a_dead_process_without_evidence_stays_a_process_failure() {
        let mut probe =
            ScriptedProbe::new(vec![Some(true), Some(false)], vec![Err("refused".into())]);
        let out = wait_for_confirm(&mut probe, "0.3.61", confirm_deadlines(true), |_| {});
        match out {
            ConfirmOutcome::Failed(f @ ConfirmFailure::ProcessGone { .. }) => {
                assert_eq!(f.signal(), "process");
            }
            other => panic!("expected ProcessGone, got {other:?}"),
        }
    }

    /// The evidence rule itself, with a fake filesystem: a deleted file beside
    /// a real directory is a quarantine; a directory that never existed is not.
    #[test]
    fn security_evidence_needs_a_vanished_file_or_an_access_denied_exit() {
        let exe = PathBuf::from(r"C:\gpn\bin\gpn-node.exe");
        let dir = PathBuf::from(r"C:\gpn\bin");

        // (a) the directory exists and the binary is gone -> quarantine shape.
        let gone = |p: &Path| p != exe.as_path();
        let ev = detect_security_block(std::slice::from_ref(&exe), &gone, "")
            .expect("missing exe is evidence");
        assert!(ev.contains("gpn-node.exe is missing"), "{ev}");

        // (b) nothing installed at all -> no evidence; this is a fresh box.
        let empty = |_p: &Path| false;
        assert_eq!(
            detect_security_block(std::slice::from_ref(&exe), &empty, ""),
            None
        );

        // (c) everything present and a silent exit -> no evidence.
        let present = |_p: &Path| true;
        assert_eq!(
            detect_security_block(std::slice::from_ref(&exe), &present, ""),
            None
        );

        // (d) STATUS_ACCESS_DENIED in the exit ledger, signed or unsigned.
        let signed = r#"{"kind":"spawn-error","exit_code":-1073741790}"#;
        let ev = detect_security_block(std::slice::from_ref(&exe), &present, signed)
            .expect("0xC0000022");
        assert!(ev.contains("0xC0000022"), "{ev}");
        let unsigned = r#"{"kind":"spawn-error","exit_code":3221225506}"#;
        assert!(detect_security_block(std::slice::from_ref(&exe), &present, unsigned).is_some());
        let hexed = r#"{"kind":"spawn-error","exit_code":"0xc0000022"}"#;
        assert!(detect_security_block(std::slice::from_ref(&exe), &present, hexed).is_some());

        // (e) STATUS_DLL_NOT_FOUND alone is the missing-VC++-runtime case, not
        // a block; only a security-software phrase makes it evidence.
        let dll = r#"{"kind":"spawn-error","exit_code":-1073741515}"#;
        assert_eq!(
            detect_security_block(std::slice::from_ref(&exe), &present, dll),
            None
        );
        let dll_av = r#"{"kind":"spawn-error","exit_code":-1073741515,"detail":"Windows Defender blocked this file"}"#;
        assert!(detect_security_block(&[exe], &present, dll_av).is_some());

        // (f) the wording Defender itself uses.
        assert!(mentions_security_software(
            "Operation did not complete successfully because the file contains a virus or potentially unwanted software"
        ));
        assert!(!mentions_security_software("thread 'main' panicked"));
    }

    /// The classifier gives the block a name and its own log line, and marks it
    /// non-actionable (a retry re-runs the same blocked file).
    #[test]
    fn a_blocked_image_is_classified_as_security_software_and_not_retried() {
        let tail = vec![
            "Windows Defender Antivirus has quarantined gpn-node.exe".to_string(),
            "Operation did not complete successfully because the file contains a virus".to_string(),
        ];
        let r = classify_start_failure(&ctx("", &tail));
        match &r {
            Remediation::BlockedBySecuritySoftware { .. } => {}
            other => panic!("expected BlockedBySecuritySoftware, got {other:?}"),
        }
        assert!(!r.is_actionable(), "a retry re-runs the same blocked file");
        let line = r.log_line();
        assert!(line.contains("blocked by security software"), "{line}");
        assert!(line.contains(r"C:\gpn\bin\gpn-node.exe"), "{line}");
    }

    // ── #207 remediation: classify, fix, retry exactly once ──────────────────

    fn ctx<'a>(last_exit: &'a str, tail: &'a [String]) -> StartFailureContext<'a> {
        StartFailureContext {
            last_exit,
            log_tail: tail,
        }
    }

    #[test]
    fn a_db_lock_held_by_another_process_is_an_orphaned_family() {
        let tail = vec![
            "[node] opening database".to_string(),
            "Error: The process cannot access the file because it is being used by another process. (os error 32)".to_string(),
        ];
        match classify_start_failure(&ctx("", &tail)) {
            Remediation::OrphanedFamily { .. } => {}
            other => panic!("expected OrphanedFamily, got {other:?}"),
        }
        assert!(classify_start_failure(&ctx("", &tail)).is_actionable());
    }

    #[test]
    fn incompatible_pruning_modes_name_the_stored_mode_to_launch_with() {
        let tail = vec![
            "Error: Incompatible pruning modes [stored: Constrained(256); requested: Archive]"
                .to_string(),
        ];
        match classify_start_failure(&ctx("", &tail)) {
            Remediation::PruningMismatch { stored, .. } => assert_eq!(stored, "256"),
            other => panic!("expected PruningMismatch, got {other:?}"),
        }
        assert_eq!(
            parse_stored_pruning("stored: Archive"),
            Some("archive".into())
        );
        assert_eq!(
            parse_stored_pruning("stored: ArchiveAll]"),
            Some("archive".into())
        );
        assert_eq!(parse_stored_pruning("nothing here"), None);
    }

    #[test]
    fn a_missing_keystore_is_not_repaired_automatically() {
        let tail = vec![
            "Error: node-key not found at /home/op/.gpn/chains/gpn/network/secret_ed25519"
                .to_string(),
        ];
        match classify_start_failure(&ctx("", &tail)) {
            Remediation::FatalConfig { cause } => assert!(cause.contains("node-key")),
            other => panic!("expected FatalConfig, got {other:?}"),
        }
        assert!(!classify_start_failure(&ctx("", &tail)).is_actionable());
    }

    #[test]
    fn an_unrecognised_exit_is_unknown_and_keeps_the_line_for_the_report() {
        let tail = vec!["thread 'main' panicked at 'index out of bounds'".to_string()];
        match classify_start_failure(&ctx("", &tail)) {
            Remediation::Unknown { detail } => assert!(detail.contains("panicked")),
            other => panic!("expected Unknown, got {other:?}"),
        }
        assert!(!classify_start_failure(&ctx("", &tail)).is_actionable());
    }

    #[test]
    fn a_port_owner_is_only_killed_when_it_is_ours() {
        let ours = classify_port_holder(9944, Some((Some(42), "gpn_node".into())));
        assert!(matches!(ours, Remediation::PortHeldByGpn { .. }));
        assert!(ours.is_actionable());
        let theirs = classify_port_holder(9944, Some((Some(7), "nginx".into())));
        assert!(matches!(theirs, Remediation::PortHeldByForeign { .. }));
        assert!(!theirs.is_actionable());
        // Unidentified is foreign: never kill blind.
        assert!(matches!(
            classify_port_holder(9944, None),
            Remediation::PortHeldByForeign { .. }
        ));
    }

    #[test]
    fn bind_errors_are_recognised_in_a_log() {
        assert!(log_says_port_conflict(
            "Error: failed to bind JSON-RPC address 127.0.0.1:9944: Address already in use"
        ));
        assert!(log_says_port_conflict(
            "Only one usage of each socket address is normally permitted"
        ));
        assert!(!log_says_port_conflict("syncing block 42"));
    }

    /// The four required remediation tests, driven by scripted probes.
    /// `ScriptedProbe` lives above; this helper runs one gate attempt after
    /// another, each with its own script.
    fn run_remediation_case(
        first: ScriptedProbe,
        second: ScriptedProbe,
        remediation: Remediation,
    ) -> (Result<String, String>, Vec<String>, usize) {
        let mut probes = vec![first, second].into_iter();
        let mut make_probe = move || -> Box<dyn ConfirmProbe> {
            Box::new(probes.next().expect("the gate asked for a third probe"))
        };
        let mut classify = || remediation.clone();
        let mut applied = 0usize;
        let mut apply = |_r: &Remediation| -> Result<(), String> {
            applied += 1;
            Ok(())
        };
        let mut loglines = Vec::new();
        let out = confirm_with_remediation(
            &mut make_probe,
            &mut classify,
            &mut apply,
            "0.3.59",
            confirm_deadlines(true),
            &mut |l| loglines.push(l.to_string()),
        );
        (out, loglines, applied)
    }

    #[test]
    fn a_stale_gpn_child_holding_the_db_lock_is_stopped_and_the_start_succeeds() {
        // First attempt: the lock error, so the process dies before confirming.
        let first = ScriptedProbe::new(
            vec![Some(true), Some(false)],
            vec![Err("connection refused".into())],
        );
        // After the family is stopped, the retry comes up and confirms.
        let second = ScriptedProbe::new(vec![Some(true)], vec![Ok("0.3.59".into())]);
        let (out, logs, applied) = run_remediation_case(
            first,
            second,
            Remediation::OrphanedFamily {
                cause: "lock held by another process".into(),
            },
        );
        assert_eq!(out, Ok("0.3.59".to_string()));
        assert_eq!(applied, 1, "exactly one remediation");
        assert!(logs.iter().any(|l| l.contains("signal=process")));
        assert!(logs.iter().any(|l| l.contains("orphaned gpn family")));
    }

    #[test]
    fn a_port_held_by_a_stale_gpn_process_is_freed_and_the_start_succeeds() {
        let first = ScriptedProbe::new(vec![Some(true)], vec![Err("connection refused".into())]);
        let second = ScriptedProbe::new(vec![Some(true)], vec![Ok("0.3.59".into())]);
        let (out, logs, applied) = run_remediation_case(
            first,
            second,
            Remediation::PortHeldByGpn {
                pid: Some(42),
                name: "gpn_node".into(),
                port: 9944,
            },
        );
        assert_eq!(out, Ok("0.3.59".to_string()));
        assert_eq!(applied, 1);
        assert!(logs
            .iter()
            .any(|l| l.contains("rpc port 9944") && l.contains("gpn_node")));
    }

    #[test]
    fn incompatible_pruning_modes_is_corrected_and_the_start_succeeds() {
        let first = ScriptedProbe::new(vec![Some(true), Some(false)], vec![Err("exited".into())]);
        let second = ScriptedProbe::new(vec![Some(true)], vec![Ok("0.3.59".into())]);
        let (out, logs, applied) = run_remediation_case(
            first,
            second,
            Remediation::PruningMismatch {
                stored: "256".into(),
                cause: "incompatible pruning modes".into(),
            },
        );
        assert_eq!(out, Ok("0.3.59".to_string()));
        assert_eq!(applied, 1);
        assert!(logs
            .iter()
            .any(|l| l.contains("pruning") && l.contains("256")));
    }

    #[test]
    fn an_unknown_exit_does_not_retry_and_keeps_the_log_line() {
        let first = ScriptedProbe::new(vec![Some(true), Some(false)], vec![Err("exited".into())]);
        let second = ScriptedProbe::new(vec![Some(true)], vec![Ok("0.3.59".into())]);
        let (out, logs, applied) = run_remediation_case(
            first,
            second,
            Remediation::Unknown {
                detail: "panicked at 'index out of bounds'".into(),
            },
        );
        let err = out.expect_err("an unknown exit must fail");
        assert_eq!(applied, 0, "nothing is applied for an unknown exit");
        assert!(err.contains("panicked at 'index out of bounds'"), "{err}");
        assert!(logs.iter().any(|l| l.contains("unrecognised exit")));
    }

    #[test]
    fn a_second_failure_after_remediation_stops_at_one_attempt() {
        let first = ScriptedProbe::new(vec![Some(true), Some(false)], vec![Err("exited".into())]);
        let second = ScriptedProbe::new(vec![Some(true), Some(false)], vec![Err("exited".into())]);
        let (out, _logs, applied) = run_remediation_case(
            first,
            second,
            Remediation::OrphanedFamily {
                cause: "lock held by another process".into(),
            },
        );
        let err = out.expect_err("the retry also failed");
        assert_eq!(applied, 1, "never loops");
        assert!(err.contains("already tried"), "{err}");
    }

    // ── #203: retries never stop ────────────────────────────────────────────

    #[test]
    fn the_retry_ladder_is_1h_2h_then_a_4h_cap() {
        assert_eq!(retry_backoff_secs(1), 3_600);
        assert_eq!(retry_backoff_secs(2), 7_200);
        assert_eq!(retry_backoff_secs(3), 14_400);
        assert_eq!(retry_backoff_secs(4), 14_400);
        assert_eq!(retry_backoff_secs(50), 14_400);
        // A 0-count (an old ledger) must still wait something sane, not forever.
        assert_eq!(retry_backoff_secs(0), 3_600);
    }

    #[test]
    fn a_failure_sets_a_retry_clock_and_a_success_clears_it() {
        let dir = td("retry-clock");
        let before = now_unix();
        let rec = record_attempt_outcome(
            &dir,
            "0.3.58",
            "boot-a",
            OUTCOME_ROLLED_BACK,
            "abc",
            "0.3.58 did not come up (rpc did not answer within 360 s)",
        )
        .unwrap();
        assert_eq!(rec.attempts, 1);
        assert_eq!(rec.next_retry_at, before + 3_600);
        assert!(rec.reason.contains("did not come up"));

        // Second failure on the same version, same boot: 2 h.
        let rec2 = record_attempt_outcome(
            &dir,
            "0.3.58",
            "boot-a",
            OUTCOME_ROLLED_BACK,
            "abc",
            "still not up",
        )
        .unwrap();
        assert_eq!(rec2.attempts, 2);
        assert!(rec2.next_retry_at >= before + 7_200);

        // Success supersedes it, and nothing is published afterwards.
        let ok =
            record_attempt_outcome(&dir, "0.3.58", "boot-a", OUTCOME_CONFIRMED, "abc", "").unwrap();
        assert_eq!(ok.next_retry_at, 0);
        assert!(
            update_retry_view(&dir).is_none(),
            "a success publishes nothing"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn retry_wait_is_zero_when_due_and_never_negative() {
        let base = UpdateAttempt {
            version: "0.3.58".into(),
            boot_id: "boot-a".into(),
            at: 100,
            outcome: OUTCOME_ROLLED_BACK.into(),
            release_id: "abc".into(),
            attempts: 1,
            reason: "boom".into(),
            next_retry_at: 1_000,
        };
        assert_eq!(retry_wait_secs(&base, "abc", 400), Some(600));
        assert_eq!(retry_wait_secs(&base, "abc", 1_000), Some(0));
        assert_eq!(retry_wait_secs(&base, "abc", 9_999), Some(0));
        // A re-cut release is not blocked at all.
        assert_eq!(retry_wait_secs(&base, "def", 400), None);
        // Neither is a success.
        let ok = UpdateAttempt {
            outcome: OUTCOME_CONFIRMED.into(),
            ..base
        };
        assert_eq!(retry_wait_secs(&ok, "abc", 400), None);
    }

    #[test]
    fn the_decline_line_names_the_time_and_the_reason() {
        assert_eq!(
            declined_text(1_765_000_000, "rpc did not answer"),
            "update declined until 2025-12-06 05:46:40 UTC (last: rpc did not answer)"
        );
        assert!(declined_text(1_765_000_000, "  ").contains("(last: no reason recorded)"));
    }

    #[test]
    fn unix_seconds_render_as_utc() {
        assert_eq!(format_unix_utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(format_unix_utc(1_000_000_000), "2001-09-09 01:46:40 UTC");
        // A leap day, and a date round the 2100 non-leap century rule.
        assert_eq!(format_unix_utc(951_782_400), "2000-02-29 00:00:00 UTC");
        assert_eq!(format_unix_utc(4_107_542_400), "2100-03-01 00:00:00 UTC");
    }

    #[test]
    fn the_status_view_appears_only_while_a_retry_is_pending() {
        let dir = td("retry-view");
        assert!(
            update_retry_view(&dir).is_none(),
            "no ledger, nothing to say"
        );
        // `started` is in flight, not pending: the confirm step is the thing
        // that consumes an attempt (#207), so a start that never reached it
        // stays retryable and is not reported as "declined until".
        record_attempt_outcome(&dir, "0.3.58", "boot-a", OUTCOME_STARTED, "abc", "started")
            .unwrap();
        assert!(
            update_retry_view(&dir).is_none(),
            "an unfinished start must not look like a scheduled retry"
        );

        record_attempt_outcome(
            &dir,
            "0.3.58",
            "boot-a",
            OUTCOME_FAILED,
            "abc",
            "did not start",
        )
        .unwrap();
        let v = update_retry_view(&dir).expect("a failed attempt is pending");
        assert_eq!(v.version, "0.3.58");
        assert_eq!(v.attempts, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The whole point of the row: no outcome, no boot id and no clock value may
    /// turn into "stop trying forever".
    #[test]
    fn a_failure_is_always_followed_by_a_retry_time() {
        let dir = td("retry-forever");
        for i in 0..12u32 {
            let r = record_attempt_outcome(
                &dir,
                "0.3.58",
                &format!("boot-{i}"),
                OUTCOME_FAILED,
                "abc",
                "download failed: HTTP 504",
            )
            .unwrap();
            assert!(
                r.next_retry_at > now_unix() - 1,
                "attempt {} must schedule a retry, got {r:?}",
                i + 1
            );
            assert_eq!(r.attempts, i + 1, "the count grows across boots too");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
