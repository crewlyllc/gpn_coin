//! `gpn_node --version` / `--help` must answer and exit **before** the
//! first-run machinery: no base dir, no config file, no chain-spec fetch, no
//! wallet prompt. These tests run the real binary in a scrubbed environment
//! and assert that nothing was created on disk.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_gpn_node");

fn scratch(tag: &str) -> PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let p = std::env::temp_dir().join(format!("gpn-cli-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// Run the binary with a clean environment; returns (status ok, stdout,
/// stderr) and the scratch HOME (which must stay empty).
fn run(args: &[&str]) -> (bool, String, String, PathBuf) {
    let home = scratch("home");
    let out = Command::new(BIN)
        .args(args)
        // No inherited GPN_* config: proves the info flags need none of it.
        .env_clear()
        .env("HOME", &home)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("GPN_BASE_PATH", home.join("base"))
        .output()
        .expect("failed to run gpn_node");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        home,
    )
}

fn assert_untouched(home: &PathBuf) {
    let entries: Vec<_> = std::fs::read_dir(home).unwrap().collect();
    assert!(
        entries.is_empty(),
        "info flags must not write anything, found: {entries:?}"
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn version_exits_zero_and_prints_version() {
    let (ok, stdout, stderr, home) = run(&["--version"]);
    assert!(ok, "--version must exit 0; stderr: {stderr}");
    assert!(
        stdout.contains(concat!("gpn_node ", env!("CARGO_PKG_VERSION"))),
        "unexpected --version output: {stdout:?}"
    );
    assert!(stdout.lines().count() == 1, "one line only: {stdout:?}");
    assert!(stderr.is_empty(), "no diagnostics expected: {stderr:?}");
    assert_untouched(&home);
}

#[test]
fn short_version_flag_works() {
    let (ok, stdout, _, home) = run(&["-V"]);
    assert!(ok);
    assert!(stdout.contains(env!("CARGO_PKG_VERSION")), "{stdout:?}");
    assert_untouched(&home);
}

/// The release tooling reads the version and git SHA out of the artifact by
/// scanning for this marker, so it must survive into the linked binary (a
/// Windows `.exe` cannot be executed to ask it).
#[test]
fn binary_carries_the_build_id_marker() {
    let bytes = std::fs::read(BIN).expect("read the built binary");
    let needle = format!("gpn-build-v1 version={} sha=", env!("CARGO_PKG_VERSION"));
    assert!(
        bytes
            .windows(needle.len())
            .any(|window| window == needle.as_bytes()),
        "binary does not contain {needle:?} — scripts/gen-release-manifest.py \
         could not derive the version from it"
    );
}

/// The version line may carry `(spec <n>, <sha>)`; whatever it carries must be
/// one line, ASCII, and not leak the `unknown` placeholder.
#[test]
fn version_line_shape_is_sane() {
    let (ok, stdout, _, home) = run(&["--version"]);
    assert!(ok);
    let line = stdout.trim_end_matches(['\r', '\n']);
    assert!(line.starts_with(concat!("gpn_node ", env!("CARGO_PKG_VERSION"))), "{line:?}");
    assert!(line.is_ascii(), "{line:?}");
    assert!(!line.contains("unknown"), "{line:?}");
    assert_eq!(line.matches('(').count(), line.matches(')').count(), "{line:?}");
    assert_untouched(&home);
}

#[test]
fn help_exits_zero_and_documents_flags() {
    let (ok, stdout, stderr, home) = run(&["--help"]);
    assert!(ok, "--help must exit 0; stderr: {stderr}");
    for needle in ["Usage:", "--version", "--help", "--setup", "GPN_BASE_PATH"] {
        assert!(
            stdout.contains(needle),
            "help is missing {needle}: {stdout:?}"
        );
    }
    assert_untouched(&home);
}

#[test]
fn short_help_flag_works() {
    let (ok, stdout, _, home) = run(&["-h"]);
    assert!(ok);
    assert!(stdout.contains("Usage:"), "{stdout:?}");
    assert_untouched(&home);
}
