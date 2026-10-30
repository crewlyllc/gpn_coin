//! `gpn_node --ref CODE` / `--ref=CODE` against the **real binary**.
//!
//! The referral file is `$GPN_PREFIX/referral` (write-once). These tests point
//! `GPN_PREFIX` at a scratch directory and pair `--ref` with `--help`, which
//! exits immediately after the referral is applied — no config load, no chain
//! spec fetch, no wallet prompt, no ollama probe.
//!
//! The published install snippets must be exactly what the binary accepts, so
//! the `--setup --ref CODE` ordering is exercised here too (argument parsing
//! only: `--help` wins before any bootstrap runs).

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_gpn_node");

fn scratch(tag: &str) -> PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let p = std::env::temp_dir().join(format!("gpn-ref-cli-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

struct Run {
    ok: bool,
    stdout: String,
    stderr: String,
}

fn run(prefix: &Path, args: &[&str]) -> Run {
    let out = Command::new(BIN)
        .args(args)
        .env_clear()
        .env("HOME", prefix)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("GPN_PREFIX", prefix)
        .env("GPN_BASE_PATH", prefix.join("base"))
        .output()
        .expect("failed to run gpn_node");
    Run {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn ref_file(prefix: &Path) -> PathBuf {
    prefix.join("referral")
}

#[test]
fn separate_value_form_is_recorded() {
    let dir = scratch("sep");
    let r = run(&dir, &["--ref", "abc123", "--help"]);
    assert!(r.ok, "must exit 0; stderr: {}", r.stderr);
    assert!(
        r.stdout.contains("recorded 'abc123'"),
        "expected a referral log line: {}{}",
        r.stdout,
        r.stderr
    );
    assert_eq!(std::fs::read_to_string(ref_file(&dir)).unwrap(), "abc123\n");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn equals_form_is_recorded() {
    let dir = scratch("eq");
    let r = run(&dir, &["--ref=abc123", "--help"]);
    assert!(r.ok, "stderr: {}", r.stderr);
    assert_eq!(std::fs::read_to_string(ref_file(&dir)).unwrap(), "abc123\n");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_published_setup_ordering_is_accepted() {
    // Exactly the Windows/Linux snippet shape: `--setup --ref CODE`.
    let dir = scratch("setup");
    let r = run(&dir, &["--setup", "--ref", "abc123", "--help"]);
    assert!(r.ok, "--setup --ref must parse; stderr: {}", r.stderr);
    assert_eq!(std::fs::read_to_string(ref_file(&dir)).unwrap(), "abc123\n");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn write_once_survives_a_second_run_with_a_different_code() {
    let dir = scratch("once");
    assert!(run(&dir, &["--ref", "team-alpha", "--help"]).ok);
    let r = run(&dir, &["--ref", "other-team", "--help"]);
    assert!(
        r.ok,
        "an existing code must never fail the run: {}",
        r.stderr
    );
    assert!(
        r.stdout.contains("already set to 'team-alpha'")
            && r.stdout.contains("ignoring --ref 'other-team'"),
        "expected the ignore log line: {}{}",
        r.stdout,
        r.stderr
    );
    assert_eq!(
        std::fs::read_to_string(ref_file(&dir)).unwrap(),
        "team-alpha\n"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_invalid_code_warns_and_creates_nothing() {
    let dir = scratch("invalid");
    let r = run(&dir, &["--ref", "ab", "--help"]);
    assert!(r.ok, "an invalid referral must not fail the run");
    assert!(
        r.stdout.contains("invalid code 'ab'") || r.stderr.contains("invalid code 'ab'"),
        "expected a warning: {}{}",
        r.stdout,
        r.stderr
    );
    assert!(!ref_file(&dir).exists(), "no file for an invalid code");

    // A shell-ish injection string is simply a bad code.
    let r = run(&dir, &["--ref", "ab\"><script>", "--help"]);
    assert!(r.ok);
    assert!(!ref_file(&dir).exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn help_documents_the_referral_flag() {
    let dir = scratch("help");
    let r = run(&dir, &["--help"]);
    assert!(r.ok);
    assert!(
        r.stdout.contains("--ref CODE") && r.stdout.contains("--ref=CODE"),
        "--help must document --ref: {}",
        r.stdout
    );
    assert!(!ref_file(&dir).exists(), "--help alone writes no referral");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn no_referral_flag_writes_nothing() {
    let dir = scratch("none");
    assert!(run(&dir, &["--help"]).ok);
    assert!(!ref_file(&dir).exists());
    let _ = std::fs::remove_dir_all(dir);
}
