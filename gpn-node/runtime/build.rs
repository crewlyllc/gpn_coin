//! Build script for the GPN runtime.
//!
//! Deterministic runtime WASM: if a canonical pre-built WASM is committed
//! (`gpn_runtime.compact.compressed.wasm` next to this file), it is embedded
//! directly via `include_bytes!` so every host platform (Linux/Windows/macOS)
//! produces an identical genesis hash. Otherwise the WASM is compiled locally.

#[cfg(all(feature = "std", not(feature = "metadata-hash")))]
fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set");
    let committed =
        std::path::Path::new(&manifest_dir).join("gpn_runtime.compact.compressed.wasm");

    if committed.exists() {
        // Forward-slash absolute path (cross-platform `include_bytes!`).
        let wasm = format!(
            "{}/gpn_runtime.compact.compressed.wasm",
            manifest_dir.replace('\\', "/")
        );

        let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo");
        let dest = std::path::Path::new(&out_dir).join("wasm_binary.rs");

        let content = format!(
            "pub const WASM_BINARY_PATH: Option<&str> = Some(\"{wasm}\");\n\
             pub const WASM_BINARY: Option<&[u8]> = Some(include_bytes!(\"{wasm}\"));\n\
             pub const WASM_BINARY_BLOATY: Option<&[u8]> = None;\n"
        );

        std::fs::write(&dest, content).expect("write wasm_binary.rs");
        println!("cargo:rerun-if-changed=gpn_runtime.compact.compressed.wasm");
        println!("cargo:warning=embedding committed canonical runtime WASM");
    } else {
        substrate_wasm_builder::WasmBuilder::build_using_defaults();
    }
}

/// The wasm builder is deactivated when compiling this crate for wasm to speed up compilation.
#[cfg(not(feature = "std"))]
fn main() {}
