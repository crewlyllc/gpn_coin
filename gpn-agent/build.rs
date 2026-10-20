//! Build script: expose the runtime `spec_version` at compile time.
//!
//! `gpn_node --version` should print `gpn_node <pkg> (spec <n>)` without
//! pulling in the (very heavy) runtime crate as a dependency. The number lives
//! in one place — `gpn-node/runtime/src/lib.rs`'s `VERSION` const — so we read
//! it here and inject it as `GPN_RUNTIME_SPEC_VERSION`.
//!
//! When the crate is built outside the monorepo (no runtime source next to it)
//! the env var is simply not set and `--version` prints the package version.

fn main() {
    let runtime_lib = "../gpn-node/runtime/src/lib.rs";
    println!("cargo:rerun-if-changed={runtime_lib}");

    if let Ok(src) = std::fs::read_to_string(runtime_lib) {
        if let Some(version) = extract_spec_version(&src) {
            println!("cargo:rustc-env=GPN_RUNTIME_SPEC_VERSION={version}");
        }
    }
}

/// First `spec_version: <n>` line in the runtime source (the `VERSION` const).
fn extract_spec_version(src: &str) -> Option<u32> {
    src.lines()
        .find_map(|line| line.trim().strip_prefix("spec_version:"))
        .and_then(|rest| rest.trim().trim_end_matches(',').parse().ok())
}
