//! Every binary this crate ships must carry the `gpn-build-v1` stamp.
//!
//! It used to be linked only into binaries that *asked* for it
//! (`firstrun::build_id()`), so `gpn_scheduler` and the providers shipped with
//! no stamp at all. That is not cosmetic: the updater's downgrade guard reads
//! the stamp of each file in the install dir to decide whether a signed release
//! may replace it, and an unstamped binary is an *unknown* it must not act on.
//! In `#148` an unstamped locally deployed `gpn_scheduler` was silently replaced
//! by an older release build for exactly this reason.
//!
//! `firstrun::GPN_BUILD_STAMP` is `#[used] #[no_mangle]`, so the literal
//! survives into every linked binary. These tests fail if that ever regresses.

use std::path::Path;

/// The stamp must be findable *and* readable as a real version (not only the
/// `version=unknown` fallback that also lives in the crate).
fn assert_stamped(name: &str, path: &Path) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {name}: {e}"));
    let needle = b"gpn-build-v1 version=";
    assert!(
        bytes.windows(needle.len()).any(|w| w == needle),
        "{name} carries no gpn-build-v1 stamp — the always-linked \
         firstrun::GPN_BUILD_STAMP went missing"
    );
    let stamp = gpn_agent::update::read_build_stamp_full(path)
        .unwrap_or_else(|e| panic!("{name}: read stamp: {e}"))
        .unwrap_or_else(|| panic!("{name} carries only the version=unknown fallback"));
    assert_eq!(
        stamp.version,
        env!("CARGO_PKG_VERSION"),
        "{name}: stamp names the wrong version"
    );
    // `sha=unknown` is allowed (a build outside a git work tree), but a real
    // 7–40 hex commit must be parsed out when present.
    if let Some(sha) = stamp.sha {
        assert!(
            sha.bytes().all(|b| b.is_ascii_hexdigit()) && (7..=40).contains(&sha.len()),
            "{name}: implausible sha {sha:?}"
        );
    }
}

#[test]
fn the_scheduler_binary_is_stamped_even_though_it_never_prints_a_version() {
    assert_stamped(
        "gpn_scheduler",
        Path::new(env!("CARGO_BIN_EXE_gpn_scheduler")),
    );
}

#[test]
fn the_updater_binary_is_stamped() {
    assert_stamped("gpn_update", Path::new(env!("CARGO_BIN_EXE_gpn_update")));
}
