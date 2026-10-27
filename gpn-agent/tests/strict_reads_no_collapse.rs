//! The lint half of the `Strict<T>` fix (#100–#111).
//!
//! The failure-path unit tests prove the *converted* sites behave. This file
//! proves they are still converted a week later: it greps the source for the
//! exact idiom each item removed, so the bug cannot silently come back in the
//! next edit to the same function.
//!
//! Deliberately grep-based: it costs nothing, runs in the normal `cargo test`
//! suite, and fails with `file:line` plus the offending text.

/// (file relative to the crate root, substrings that must NOT appear in
/// non-comment code)
const FORBIDDEN: &[(&str, &[&str])] = &[
    // #110: the signing nonce must never be read through the value form; a
    // failed read used to be signed into a transaction as 0.
    ("src/chain/mod.rs", &[".get_account_nonce("]),
    // #108: the profitability decision must not read difficulty/feed through
    // the fallback-constant forms; a failed read became FALLBACK_DIFFICULTY.
    (
        "src/profitability/mod.rs",
        &[".get_difficulty().await", ".get_feed().await"],
    ),
    // #108/#110: registration spent a bond and advertised `1 GPU / 0 GFLOPS`
    // for a chain it never read.
    ("src/main.rs", &["unwrap_or((1, 0))"]),
    // #110 in the API: an unreadable chain was published as
    // `staked: false, attested: false, tier: "Unattested"`.
    ("src/bin/gpn_api.rs", &["unwrap_or((false, false))"]),
    // #65/#108 in the heartbeat: an unreadable attestation fell through to the
    // operator's typed `--gflops` and was advertised to the router.
    (
        "src/bin/gpn_provider_status.rs",
        &["Option<u64> {\n    if let Ok(guard) = attest_cache"],
    ),
    // The same collapse in the two serving loops: a failed attestation read
    // became 0 GFLOPS and narrowed the claim window to min_gflops = 0.
    ("src/bin/gpn_llm_provider.rs", &[".map(|(_, f, _)| f)"]),
    (
        "src/bin/gpn_diffusion_provider.rs",
        &[".map(|(_, f, _)| f)"],
    ),
];

/// Files that carry the strict API and are expected to mention it, so a
/// well-meaning "simplification" that drops it is caught too.
const MUST_USE_STRICT: &[(&str, &str)] = &[
    ("src/chain/mod.rs", "Strict<Option<OnChainFeed>>"),
    ("src/profitability/mod.rs", "get_difficulty_strict"),
    ("src/main.rs", "claim_gate"),
    ("src/bin/gpn_api.rs", "chain_read_error"),
    ("src/bin/gpn_provider_status.rs", "Strict<Option<u64>>"),
    ("src/bin/gpn_llm_provider.rs", "strict::Strict::from_result"),
    (
        "src/bin/gpn_diffusion_provider.rs",
        "strict::Strict::from_result",
    ),
];

#[test]
fn converted_sites_do_not_use_the_collapsing_idiom() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut failures = Vec::new();
    for (rel, forbidden) in FORBIDDEN {
        let path = root.join(rel);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        for (i, line) in text.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            for pat in *forbidden {
                if line.contains(pat) {
                    failures.push(format!(
                        "{}:{}: {} (found `{pat}`)",
                        rel,
                        i + 1,
                        line.trim()
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "a read that can fail is being collapsed into a normal value again:\n{}",
        failures.join("\n")
    );
}

#[test]
fn the_strict_api_is_still_wired_into_the_converted_files() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let missing: Vec<String> = MUST_USE_STRICT
        .iter()
        .filter_map(|(rel, needle)| {
            let text = std::fs::read_to_string(root.join(rel)).ok()?;
            if text.contains(needle) {
                None
            } else {
                Some(format!("{rel}: missing `{needle}`"))
            }
        })
        .collect();
    assert!(
        missing.is_empty(),
        "the strict read was removed from a converted site:\n{}",
        missing.join("\n")
    );
}

/// `Strict` must not grow the escape hatch that made the bug possible.
#[test]
fn strict_has_no_silent_unwrap_or() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(root.join("src/strict.rs")).unwrap();
    for forbidden in [
        "pub fn unwrap_or",
        "pub fn unwrap_or_default",
        "pub fn unwrap_or_else",
    ] {
        // `or_default_for_display_only` is the named seam; a method that reads
        // like `Option::unwrap_or` must not exist.
        assert!(
            !text.contains(forbidden),
            "Strict grew `{forbidden}` — that is the API shape this type exists to remove"
        );
    }
}
