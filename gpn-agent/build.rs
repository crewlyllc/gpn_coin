//! Build script: expose the runtime `spec_version`, the git SHA, and a single
//! machine-readable build stamp at compile time.
//!
//! `gpn_node --version` should print `gpn_node <pkg> (spec <n>, <sha>)` without
//! pulling in the (very heavy) runtime crate as a dependency, and
//! `scripts/gen-release-manifest.py` must be able to read the **version and SHA
//! an artifact was actually built from** — including for a Windows `.exe` that
//! cannot be executed on the release host. Both are served by one string:
//!
//! ```text
//! GPN_BUILD_ID = "gpn-build-v1 version=0.2.0 sha=d606264 spec=41"
//! ```
//!
//! It is injected as `cargo:rustc-env`, so it is a plain string literal in the
//! binary and `strings`/a byte scan of any artifact (or of a `.exe`) finds it.
//! Read it through `gpn_agent::firstrun::build_id()`; do not re-format it in a
//! second place.
//!
//! Every field degrades independently:
//!
//! | field | source | absent ⇒ |
//! |---|---|---|
//! | `version` | `CARGO_PKG_VERSION` | never absent |
//! | `sha` | `$GPN_GIT_SHA`, else `git rev-parse --short=7 HEAD` | `unknown` |
//! | `spec` | `gpn-node/runtime/src/lib.rs`'s `VERSION` const | field omitted |
//!
//! `GPN_GIT_SHA` is also injected on its own (`gpn_agent::buildinfo::GIT_SHA`,
//! `option_env!`) for callers that only want the SHA.
//!
//! Nothing here may fail the build: a tarball release, a vendored crate, or a
//! container without `git` must still compile.

use std::process::Command;

const BUILD_ID_PREFIX: &str = "gpn-build-v1";

fn main() {
    println!("cargo:rerun-if-env-changed=GPN_GIT_SHA");

    let spec = runtime_spec_version();
    let sha = git_sha();

    let mut stamp = format!(
        "{BUILD_ID_PREFIX} version={} sha={sha}",
        std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "unknown".into())
    );
    if let Some(spec) = spec.as_deref() {
        stamp.push_str(&format!(" spec={spec}"));
        println!("cargo:rustc-env=GPN_RUNTIME_SPEC_VERSION={spec}");
    }
    println!("cargo:rustc-env=GPN_BUILD_ID={stamp}");
    if sha != "unknown" {
        println!("cargo:rustc-env=GPN_GIT_SHA={sha}");
    }
}

/// `gpn-node/runtime/src/lib.rs` → the `spec_version` from its `VERSION` const.
fn runtime_spec_version() -> Option<String> {
    let runtime_lib = "../gpn-node/runtime/src/lib.rs";
    println!("cargo:rerun-if-changed={runtime_lib}");

    let src = std::fs::read_to_string(runtime_lib).ok()?;
    extract_spec_version(&src).map(|v| v.to_string())
}

/// First `spec_version: <n>` line in the runtime source (the `VERSION` const).
fn extract_spec_version(src: &str) -> Option<u32> {
    src.lines()
        .find_map(|line| line.trim().strip_prefix("spec_version:"))
        .and_then(|rest| rest.trim().trim_end_matches(',').parse().ok())
}

/// Short git SHA, or `"unknown"` outside a git checkout.
///
/// **Never** fails the build; `--version` and the release script both tolerate
/// `unknown`.
///
/// No `-dirty` suffix on purpose: the stamp must identify a commit (it is what a
/// release manifest pins), and a local dirty worktree would otherwise leak into
/// every build — including test builds — and make builds non-reproducible.
fn git_sha() -> String {
    // A CI/release build can pin the SHA when `.git` is not there.
    if let Ok(pinned) = std::env::var("GPN_GIT_SHA") {
        let pinned = pinned.trim().to_owned();
        if is_sha(&pinned) {
            return pinned;
        }
        if !pinned.is_empty() {
            println!("cargo:warning=GPN_GIT_SHA={pinned:?} is not 7-40 hex chars; ignoring it");
        }
    }

    // Re-run when HEAD moves (branch switch, commit) so the SHA is never stale.
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs/heads");

    let out = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .current_dir(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into()))
        .output();

    match out {
        Ok(o) if o.status.success() => {
            let sha = String::from_utf8_lossy(&o.stdout).trim().to_owned();
            if is_sha(&sha) {
                sha
            } else {
                "unknown".into()
            }
        }
        // No git binary, no repo, no commits yet — all normal, all quiet.
        _ => "unknown".into(),
    }
}

/// Hex SHA, 7..=40 chars. Guards against `git` printing an error string or a
/// localised message instead of a hash.
fn is_sha(s: &str) -> bool {
    (7..=40).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}
