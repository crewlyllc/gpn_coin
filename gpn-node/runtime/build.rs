//! Build script for the GPN runtime.
//!
//! Deterministic runtime WASM: if a canonical pre-built WASM is committed
//! (`gpn_runtime.compact.compressed.wasm` next to this file), it is embedded
//! directly via `include_bytes!` so every host platform (Linux/Windows/macOS)
//! produces an identical genesis hash. Otherwise the WASM is compiled locally.
//!
//! The committed bytes are copied into `OUT_DIR` and included by a path
//! **relative to the generated file**, never by an absolute path. `include_bytes!`
//! bakes its argument into `wasm_binary.rs`, and that file lives in a shared
//! cargo cache (`CARGO_TARGET_DIR=~/.ci-cache/gpn-node-target` on this fleet), so
//! an absolute path into a throwaway worktree outlives the worktree: the next
//! build in a different checkout re-uses the cached generated file and fails with
//! `couldn't read /tmp/pm07-rel0350/gpn-node/runtime/gpn_runtime.compact.compressed.wasm`
//! — a red CI that has nothing to do with the code (2026-11-16, twice: `/tmp/pm07-rel0350`
//! and `/tmp/pm07-w189`). Relative-to-`OUT_DIR` cannot go stale: the copy is
//! re-made by this script, and cargo re-runs it when the canonical WASM, this
//! script, or the manifest directory changes.

#[cfg(all(feature = "std", not(feature = "metadata-hash")))]
fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set");
    let committed = std::path::Path::new(&manifest_dir).join("gpn_runtime.compact.compressed.wasm");

    if committed.exists() {
        let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo");
        let dest = std::path::Path::new(&out_dir).join("wasm_binary.rs");

        // The copy is what gets embedded; `wasm_binary.rs` is written next to it,
        // so `include_bytes!("canonical_runtime.wasm")` resolves inside OUT_DIR.
        let embedded = std::path::Path::new(&out_dir).join("canonical_runtime.wasm");
        std::fs::copy(&committed, &embedded).expect("copy canonical runtime WASM into OUT_DIR");
        let src_path = format!(
            "{}/gpn_runtime.compact.compressed.wasm",
            manifest_dir.replace('\\', "/")
        );

        let content = format!(
            "/// Where the embedded bytes came from (diagnostics only).\n\
             pub const WASM_BINARY_PATH: Option<&str> = Some(\"{src_path}\");\n\
             /// Relative to this file (OUT_DIR) — deliberately not an absolute path.\n\
             pub const WASM_BINARY: Option<&[u8]> = Some(include_bytes!(\"canonical_runtime.wasm\"));\n\
             pub const WASM_BINARY_BLOATY: Option<&[u8]> = None;\n"
        );

        std::fs::write(&dest, content).expect("write wasm_binary.rs");
        println!("cargo:rerun-if-changed=gpn_runtime.compact.compressed.wasm");
        println!("cargo:warning=embedding committed canonical runtime WASM (copied into OUT_DIR)");
    } else {
        substrate_wasm_builder::WasmBuilder::build_using_defaults();
    }
}

/// The wasm builder is deactivated when compiling this crate for wasm to speed up compilation.
#[cfg(not(feature = "std"))]
fn main() {}
