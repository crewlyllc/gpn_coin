//! End-to-end tests for `gpn_node --diagnose` and the one fatal exit path.
//!
//! These run the real binary (its path is `CARGO_BIN_EXE_gpn_node`) with
//! environment injection, so they assert the operator-visible behaviour: the
//! PASS/FAIL lines, the exact fix text, and the `last-exit.json` entry.
//!
//! Every test uses its own temp base path and `GPN_DIAGNOSE_PORTS`, so a real
//! node running on 9944/30333 can never make them flaky.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_gpn_node");

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let p = std::env::temp_dir().join(format!("gpn-diag-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// Remove every GPN_* variable that could leak in from the CI shell.
fn clean(cmd: &mut Command, dir: &Path) {
    for k in [
        "GPN_BASE_PATH",
        "GPN_EXIT_LOG",
        "GPN_PREFIX",
        "GPN_OLLAMA",
        "GPN_CHAIN",
        "GPN_SIGNER",
        "GPN_SIGNER_FILE",
        "GPN_NODE_KEY",
        "GPN_NODE_KEY_FILE",
        "GPN_DIAGNOSE",
        "GPN_DIAGNOSE_PORTS",
        "GPN_DISK_MIN_PERCENT",
        "GPN_REQUIRE_OLLAMA",
        "GPN_ENSURE_WALLET",
        "GPN_PAYOUT_ACCOUNT",
        "GPN_NODE_ARGS",
    ] {
        cmd.env_remove(k);
    }
    cmd.env("GPN_BASE_PATH", dir.join("node"))
        .env("GPN_EXIT_LOG", dir.join("last-exit.json"))
        .env("GPN_NODE_KEY_FILE", dir.join("node-key"))
        .env("GPN_DIAGNOSE_PORTS", free_ports_csv());
}

/// Five free loopback ports as a CSV, so the port check cannot collide with a
/// node that is really running.
fn free_ports_csv() -> String {
    let mut ports = Vec::new();
    for _ in 0..5 {
        let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        ports.push(l.local_addr().unwrap().port().to_string());
    }
    ports.join(",")
}

fn diagnose(dir: &Path, extra: &[(&str, String)]) -> Output {
    let mut cmd = Command::new(BIN);
    clean(&mut cmd, dir);
    cmd.arg("--diagnose");
    for (k, v) in extra {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// A throwaway HTTP server that answers every request with `body`.
fn serve_once(body: &'static str) -> u16 {
    let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in l.incoming().take(16) {
            let Ok(mut s) = stream else { continue };
            let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(2)));
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = s.write_all(resp.as_bytes());
        }
    });
    port
}

#[test]
fn diagnose_fails_loudly_with_a_fix_for_every_missing_prerequisite() {
    let dir = temp_dir("fail");
    let out = diagnose(
        &dir,
        &[
            ("GPN_OLLAMA", "http://127.0.0.1:1".to_string()),
            ("GPN_CHAIN", "testnet".to_string()),
        ],
    );
    let t = text(&out);
    assert_eq!(out.status.code(), Some(1), "{t}");

    for needle in [
        "[FAIL] ollama",
        "fix: gpn_node --setup",
        "[FAIL] wallet",
        "fix: gpn_node --ensure-wallet",
        "[FAIL] node-key",
        "fix: gpn-node key generate-node-key",
    ] {
        assert!(t.contains(needle), "missing {needle:?} in:\n{t}");
    }
    assert!(t.contains("prevent a start"), "{t}");
}

/// Item #130: a FAIL that does not stop the node from starting must not claim
/// it does, and must not change the exit code. On a fleet-managed host the
/// shared node key is deliberate and a held port may be the running node.
#[test]
fn diagnose_reports_advisory_fails_without_claiming_the_node_cannot_start() {
    let dir = temp_dir("advisory");
    let port = serve_once(r#"{"models":[{"name":"llama3.2:1b"}]}"#);

    // Every blocking prerequisite satisfied: a real keystore, ollama serving,
    // free ports, a good spec and a usable disk floor.
    let ks = gpn_agent::firstrun::generate_keystore();
    let ks_path = dir.join("keystore.json");
    gpn_agent::firstrun::write_keystore(&ks_path, &ks).unwrap();

    // …but the node key is the shared fleet key, which is advisory.
    let key_path = dir.join("node-key");
    std::fs::write(
        &key_path,
        "4ecac9dd3ce8f61ac2216dec9c366fb490a0d946db37947e05a4dc4de6ab543c",
    )
    .unwrap();

    let out = diagnose(
        &dir,
        &[
            ("GPN_OLLAMA", format!("http://127.0.0.1:{port}")),
            ("GPN_SIGNER_FILE", ks_path.to_string_lossy().into_owned()),
            ("GPN_NODE_KEY_FILE", key_path.to_string_lossy().into_owned()),
            ("GPN_CHAIN", "testnet".to_string()),
            ("GPN_DISK_MIN_PERCENT", "0".to_string()),
        ],
    );
    let t = text(&out);
    assert_eq!(
        out.status.code(),
        Some(0),
        "advisory-only FAILs must exit 0:\n{t}"
    );
    assert!(t.contains("[WARN] node-key"), "{t}");
    assert!(!t.contains("[FAIL]"), "{t}");
    assert!(t.contains("nothing here prevents a start"), "{t}");
    assert!(!t.contains("will not start until those are fixed"), "{t}");
}

/// A fleet-managed node pays out to a configured account, not a local keystore
/// (#130): `GPN_PAYOUT_ACCOUNT` must read as a pass, not a missing wallet.
#[test]
fn diagnose_accepts_a_fleet_payout_account_as_a_wallet() {
    let dir = temp_dir("payout");
    let port = serve_once(r#"{"models":[{"name":"llama3.2:1b"}]}"#);
    let ks = gpn_agent::firstrun::generate_keystore();
    let key_path = dir.join("node-key");
    std::fs::write(&key_path, "b".repeat(64)).unwrap();

    let out = diagnose(
        &dir,
        &[
            ("GPN_OLLAMA", format!("http://127.0.0.1:{port}")),
            ("GPN_NODE_KEY_FILE", key_path.to_string_lossy().into_owned()),
            ("GPN_PAYOUT_ACCOUNT", ks.address.clone()),
            ("GPN_CHAIN", "testnet".to_string()),
            ("GPN_DISK_MIN_PERCENT", "0".to_string()),
        ],
    );
    let t = text(&out);
    assert_eq!(out.status.code(), Some(0), "{t}");
    assert!(t.contains("[PASS] wallet"), "{t}");
    assert!(t.contains("fleet payout"), "{t}");
}

#[test]
fn diagnose_passes_only_when_every_prerequisite_is_present() {
    let dir = temp_dir("pass");
    let port = serve_once(r#"{"models":[{"name":"llama3.2:1b"}]}"#);

    // A real, self-consistent keystore.
    let ks = gpn_agent::firstrun::generate_keystore();
    let ks_path = dir.join("keystore.json");
    gpn_agent::firstrun::write_keystore(&ks_path, &ks).unwrap();

    // A per-machine node key (64 hex chars, not the shared fleet key).
    let key_path = dir.join("node-key");
    std::fs::write(&key_path, "a".repeat(64)).unwrap();

    let out = diagnose(
        &dir,
        &[
            ("GPN_OLLAMA", format!("http://127.0.0.1:{port}")),
            ("GPN_SIGNER_FILE", ks_path.to_string_lossy().into_owned()),
            ("GPN_NODE_KEY_FILE", key_path.to_string_lossy().into_owned()),
            ("GPN_CHAIN", "testnet".to_string()),
            ("GPN_DISK_MIN_PERCENT", "0".to_string()),
        ],
    );
    let t = text(&out);
    assert_eq!(out.status.code(), Some(0), "expected all-pass:\n{t}");
    assert!(t.contains("all ") && t.contains("checks passed"), "{t}");
    assert!(!t.contains("[FAIL]"), "{t}");
}

#[test]
fn diagnose_rejects_a_chain_spec_with_the_wrong_genesis() {
    let dir = temp_dir("spec");
    let bad = dir.join("bad-spec.json");
    std::fs::write(&bad, r#"{"name":"not-gpn","genesis":{}}"#).unwrap();
    let out = diagnose(
        &dir,
        &[
            ("GPN_OLLAMA", "http://127.0.0.1:1".to_string()),
            ("GPN_CHAIN", bad.to_string_lossy().into_owned()),
        ],
    );
    let t = text(&out);
    assert_eq!(out.status.code(), Some(1), "{t}");
    assert!(t.contains("[FAIL] chain-spec"), "{t}");
    assert!(t.contains("fix:"), "{t}");
}

#[test]
fn a_corrupt_keystore_exits_3_and_records_a_remediation() {
    let dir = temp_dir("corrupt");
    let ks = dir.join("keystore.json");
    std::fs::write(&ks, "{ this is not a keystore }").unwrap();

    let mut cmd = Command::new(BIN);
    clean(&mut cmd, &dir);
    cmd.arg("--ensure-wallet").env("GPN_SIGNER_FILE", &ks);
    let out = cmd.output().unwrap();
    let t = text(&out);
    assert_eq!(out.status.code(), Some(3), "{t}");
    assert!(t.contains("GPN NODE STOPPED"), "{t}");

    let log = std::fs::read_to_string(dir.join("last-exit.json")).unwrap();
    let last: serde_json::Value = log
        .lines()
        .last()
        .map(|l| serde_json::from_str(l).unwrap())
        .expect("a last-exit.json entry");
    assert_eq!(last["code"], 3);
    assert_eq!(last["kind"], "corrupt-key");
    assert!(!last["remediation"].as_str().unwrap().trim().is_empty());
    assert!(!last["detail"].as_str().unwrap().trim().is_empty());
}
