//! Integration test: a **synthetic forked node heals itself**.
//!
//! Box 1 is one instance of the divergence bug, not the test. This file builds a
//! node base path whose block hash at the signed checkpoint height is wrong,
//! serves that answer over JSON-RPC, and runs the production schedule
//! ([`gpn_agent::selfheal::watch`]) with a real child process and a real archive
//! import step. It asserts the whole sequence an operator would expect:
//!
//! * the check runs, names both hashes and the verdict;
//! * the node is stopped **before** the DB is touched;
//! * `chains/` is set aside under `chains-fork-<ts>` (never deleted);
//! * the signed snapshot is imported into a fresh `chains/`;
//! * the ledger records the recovery, so a second one is refused for 24 h;
//! * the node is started again.
//!
//! Then the four "must not wipe" failure modes, through the same entry point:
//! unverifiable metadata, an unreachable snapshot host, a stop that fails, and a
//! ledger that already recorded a heal.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ed25519_dalek::Signer;
use gpn_agent::selfheal::{
    self, CheckConfig, NodeControl, SnapshotMeta, WatchConfig, SNAPSHOT_CANONICAL_PREFIX,
};

const GENESIS: &str = "0xb8e4636a00000000000000000000000000000000000000000000000000000001";
const CANON: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const FORK: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

// ------------------------------------------------------------------ fixtures

struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("gpn-heal-it-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Tmp(d)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A node base path that looks like a real forked node: `chains/<chain>/db/full`
/// with the forked DB's files, and a sentinel we can look for afterwards.
fn forked_node_base(root: &Path) -> PathBuf {
    let base = root.join("node");
    let db = base
        .join("chains")
        .join("gpn_testnet")
        .join("db")
        .join("full");
    std::fs::create_dir_all(&db).unwrap();
    std::fs::write(db.join("CURRENT"), b"FORKED-DB").unwrap();
    std::fs::write(db.join("000123.sst"), b"fork").unwrap();
    base
}

/// Sign a `snapshot.json` whose checkpoint is `block`/`block_hash` and whose
/// archive is `archive_url` (size/hash are of a tiny fixture archive).
fn signed_meta(
    dir: &Path,
    block: u64,
    block_hash: &str,
    archive: &Path,
    archive_url: &str,
) -> (PathBuf, String) {
    let sk = ed25519_dalek::SigningKey::from_bytes(&[42u8; 32]);
    let pk = hex::encode(sk.verifying_key().to_bytes());
    let sha = selfheal::sha256_file(archive).unwrap();
    let mut m = SnapshotMeta {
        format: SNAPSHOT_CANONICAL_PREFIX.into(),
        url: archive_url.into(),
        sha256: sha,
        size: std::fs::metadata(archive).unwrap().len(),
        block: Some(block),
        genesis: GENESIS.into(),
        block_hash: block_hash.into(),
        checkpoints: Vec::new(),
        built: "2026-11-13T00:00:00Z integration-test".into(),
        signing_key: pk.clone(),
        signature: String::new(),
    };
    m.signature = hex::encode(sk.sign(&m.canonical_text()).to_bytes());
    let path = dir.join("snapshot.json");
    std::fs::write(&path, serde_json::to_vec(&m).unwrap()).unwrap();
    (path, pk)
}

/// JSON-RPC stub: genesis, best height, and the local hash at any height.
fn rpc_stub(height: u64, hash_at_checkpoint: &str) -> String {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let hash = hash_at_checkpoint.to_string();
    std::thread::spawn(move || loop {
        let Ok((mut s, _)) = listener.accept() else {
            return;
        };
        let mut reader = BufReader::new(s.try_clone().unwrap());
        let mut len = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                break;
            }
            if let Some(v) = line
                .to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().to_string())
            {
                len = v.parse().unwrap_or(0);
            }
        }
        let mut body = vec![0u8; len];
        if reader.read_exact(&mut body).is_err() {
            continue;
        }
        let req: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or_default();
        let result = match method {
            "chain_getBlockHash" => match params.get(0).and_then(|v| v.as_u64()) {
                Some(0) => serde_json::json!(GENESIS),
                _ => serde_json::json!(hash),
            },
            "chain_getHeader" => serde_json::json!({"number": format!("0x{height:x}")}),
            _ => serde_json::Value::Null,
        };
        let resp = serde_json::json!({"jsonrpc":"2.0","id":1,"result":result}).to_string();
        let _ = s.write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{resp}",
                resp.len()
            )
            .as_bytes(),
        );
        let _ = s.flush();
    });
    url
}

/// A real child process the supervisor "owns", plus a control that stops it.
struct FakeNode {
    child: Arc<Mutex<Option<Child>>>,
    events: Arc<Mutex<Vec<&'static str>>>,
    stop_outcome: Result<(), String>,
}

impl FakeNode {
    fn spawn(events: Arc<Mutex<Vec<&'static str>>>, stop_outcome: Result<(), String>) -> Self {
        // Detach the child from our stdio: a test harness that pipes stdout must
        // not be able to block on a process it does not own.
        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.args(["/c", "ping -n 120 127.0.0.1 > nul"]);
            c
        } else {
            let mut c = Command::new("sleep");
            c.arg("120");
            c
        };
        let child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn fake node child");
        FakeNode {
            child: Arc::new(Mutex::new(Some(child))),
            events,
            stop_outcome,
        }
    }
    fn alive(&self) -> bool {
        let mut g = self.child.lock().unwrap();
        match g.as_mut() {
            Some(c) => matches!(c.try_wait(), Ok(None)),
            None => false,
        }
    }
}

impl NodeControl for FakeNode {
    fn stop(&self) -> Result<(), String> {
        self.events.lock().unwrap().push("stop");
        if let Err(e) = &self.stop_outcome {
            return Err(e.clone());
        }
        // The real supervisor SIGTERMs, escalates, and only reports "stopped"
        // once the child is gone. Poll — never block in wait() while holding the
        // handle in a test.
        let mut g = self.child.lock().unwrap();
        if let Some(c) = g.as_mut() {
            let _ = c.kill();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                match c.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if std::time::Instant::now() < deadline => {
                        std::thread::sleep(std::time::Duration::from_millis(20))
                    }
                    Ok(None) => return Err("fake child ignored SIGKILL".into()),
                    Err(e) => return Err(format!("try_wait: {e}")),
                }
            }
        }
        *g = None;
        Ok(())
    }
    fn start(&self) -> Result<(), String> {
        self.events.lock().unwrap().push("start");
        Ok(())
    }
}

struct Sink(Arc<Mutex<Vec<String>>>);

impl Sink {
    fn new() -> Self {
        Sink(Arc::new(Mutex::new(Vec::new())))
    }
    fn log(&self) -> impl Fn(&str) {
        let s = self.0.clone();
        move |l: &str| s.lock().unwrap().push(l.to_string())
    }
    fn joined(&self) -> String {
        self.0.lock().unwrap().join("\n")
    }
}

/// Write a "canonical" DB the test can recognise, standing in for the archive.
fn do_import(snapshot: &Path, base: &Path) -> Result<(), String> {
    assert!(snapshot.is_file(), "snapshot must exist before import");
    let db = base
        .join("chains")
        .join("gpn_testnet")
        .join("db")
        .join("full");
    std::fs::create_dir_all(&db).map_err(|e| e.to_string())?;
    std::fs::write(db.join("CURRENT"), b"CANONICAL-DB").map_err(|e| e.to_string())
}

fn watch_cfg(tmp: &Tmp, base: &Path, rpc: String, meta: &Path, pk: &str) -> WatchConfig {
    WatchConfig {
        check: CheckConfig {
            rpc,
            base: base.to_path_buf(),
            meta_src: meta.display().to_string(),
            pubkey_hex: pk.to_string(),
            ledger: None,
        },
        snapshot_dir: tmp.path().join("snapshots"),
        interval_secs: 1,
        start_delay_secs: 0,
    }
}

// ------------------------------------------------------------------ the proof

#[test]
fn a_forked_node_is_stopped_moved_and_recovered_by_the_production_schedule() {
    let tmp = Tmp::new("heal");
    let base = forked_node_base(tmp.path());
    let archive = tmp.path().join("snap.tar.gz");
    std::fs::write(&archive, b"not really a tarball, but the digest is real").unwrap();
    // The archive is served from a file path, so no network is involved.
    let (meta, pk) = signed_meta(
        tmp.path(),
        297_000,
        CANON,
        &archive,
        &archive.display().to_string(),
    );
    let rpc = rpc_stub(297_100, FORK);

    let events = Arc::new(Mutex::new(Vec::new()));
    let node = Arc::new(FakeNode::spawn(events.clone(), Ok(())));
    assert!(node.alive());
    let sink = Sink::new();
    let stop = Arc::new(AtomicBool::new(false));

    // `watch` is what the supervisor thread runs; it stops after the recovery
    // because the import sets the stop flag (a real node keeps going).
    let cfg = watch_cfg(&tmp, &base, rpc, &meta, &pk);
    let import_events = events.clone();
    let import_stop = stop.clone();
    let base_for_import = base.clone();
    let import = move |snapshot: &Path, b: &Path| {
        import_events.lock().unwrap().push("import");
        import_stop.store(true, Ordering::SeqCst);
        do_import(snapshot, b)
    };
    let up = || true;
    let not_busy = || false;
    let sleep = |_secs: u64| std::thread::sleep(std::time::Duration::from_millis(50));
    let stop2 = stop.clone();
    let sink2 = sink.0.clone();
    let node2 = node.clone();
    let h = std::thread::spawn(move || {
        let sink = Sink(sink2);
        selfheal::watch(
            cfg,
            node2.as_ref(),
            &import,
            &up,
            &not_busy,
            &stop2,
            &sleep,
            &sink.log(),
        );
    });
    // The schedule must reach a decision on its own; the test only bounds it so
    // a regression is a failure, not a hung suite.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while std::time::Instant::now() < deadline && !stop.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    stop.store(true, Ordering::SeqCst);
    h.join().unwrap();

    let log = sink.joined();
    // Both INFO lines, naming the hashes and the verdict.
    assert!(log.contains("divergence check:"), "{log}");
    assert!(log.contains("#297100") && log.contains("#297000"), "{log}");
    assert!(
        log.contains("divergence verdict: wrong-chain")
            && log.contains(FORK)
            && log.contains(CANON),
        "{log}"
    );
    assert!(log.contains("self-heal: done"), "{log}");

    // Order: stop → import → start.
    assert_eq!(
        *events.lock().unwrap(),
        vec!["stop", "import", "start"],
        "recovery must stop the node before touching the DB"
    );
    assert!(!node.alive(), "the child that was stopped is gone");

    // The forked DB is preserved, not overwritten.
    let base_events: Vec<String> = std::fs::read_dir(&base_for_import)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with("chains-fork-"))
        .collect();
    assert_eq!(
        base_events.len(),
        1,
        "one chains-fork-<ts> dir: {base_events:?}"
    );
    let fork_dir = base_for_import.join(&base_events[0]);
    let forked = std::fs::read(fork_dir.join("gpn_testnet/db/full/CURRENT")).unwrap();
    assert_eq!(forked, b"FORKED-DB", "the fork is still there for evidence");

    // The canonical DB is in place…
    let current =
        std::fs::read(base_for_import.join("chains/gpn_testnet/db/full/CURRENT")).unwrap();
    assert_eq!(current, b"CANONICAL-DB");

    // …and the ledger records it, so a second heal inside 24 h is refused.
    let rec = selfheal::read_recovery(&selfheal::ledger_path(&base_for_import))
        .unwrap()
        .expect("ledger");
    assert!(rec.completed);
    assert_eq!(rec.checkpoint_block, 297_000);
}

// ------------------------------------------------- failure modes: never wipe

/// Common harness: a forked node + a control, and an import that must not run.
fn assert_no_recovery(tmp: &Tmp, base: &Path, rpc: String, meta: &Path, pk: &str, why: &str) {
    let sink = Sink::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    let node = FakeNode::spawn(events.clone(), Ok(()));
    let cfg = watch_cfg(tmp, base, rpc, meta, pk);
    let imported = Arc::new(AtomicBool::new(false));
    let imported2 = imported.clone();
    let import = move |_s: &Path, _b: &Path| -> Result<(), String> {
        imported2.store(true, Ordering::SeqCst);
        Ok(())
    };
    let up = || true;
    let not_busy = || false;
    let stop = Arc::new(AtomicBool::new(false));
    let sleep = |_secs: u64| std::thread::sleep(std::time::Duration::from_millis(20));
    // One tick, exactly as the schedule calls it.
    let _ = selfheal::tick(&cfg, &node, &import, &sink.log());
    assert!(
        !imported.load(Ordering::SeqCst),
        "{why}: the DB must not be replaced"
    );
    assert!(
        !events.lock().unwrap().contains(&"stop"),
        "{why}: the node must not even be stopped"
    );
    assert!(node.alive(), "{why}: the node keeps running");
    assert!(
        std::fs::read(base.join("chains/gpn_testnet/db/full/CURRENT")).unwrap() == b"FORKED-DB",
        "{why}: the DB is untouched"
    );
    assert!(
        selfheal::read_recovery(&selfheal::ledger_path(base))
            .unwrap()
            .is_none(),
        "{why}: no recovery may be recorded"
    );
    assert!(
        sink.joined().contains("divergence"),
        "{why}: it must say why"
    );
    drop(stop);
}

#[test]
fn unverifiable_metadata_never_wipes() {
    let tmp = Tmp::new("unsigned");
    let base = forked_node_base(tmp.path());
    let archive = tmp.path().join("snap.tar.gz");
    std::fs::write(&archive, b"x").unwrap();
    let (meta, pk) = signed_meta(
        tmp.path(),
        297_000,
        CANON,
        &archive,
        &archive.display().to_string(),
    );
    // Edit a signed field: the document no longer verifies.
    let tampered = std::fs::read_to_string(&meta)
        .unwrap()
        .replace("297000", "297001");
    std::fs::write(&meta, tampered).unwrap();
    assert_no_recovery(
        &tmp,
        &base,
        rpc_stub(297_100, FORK),
        &meta,
        &pk,
        "tampered metadata",
    );
}

#[test]
fn an_unreachable_snapshot_host_never_wipes() {
    let tmp = Tmp::new("nohost");
    let base = forked_node_base(tmp.path());
    let archive = tmp.path().join("snap.tar.gz");
    std::fs::write(&archive, b"payload").unwrap();
    // Signed correctly, but the archive is on a host nothing answers on.
    let (meta, pk) = signed_meta(
        tmp.path(),
        297_000,
        CANON,
        &archive,
        "http://127.0.0.1:1/x.tar.gz",
    );
    let sink = Sink::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    let node = FakeNode::spawn(events.clone(), Ok(()));
    let cfg = watch_cfg(&tmp, &base, rpc_stub(297_100, FORK), &meta, &pk);
    let import = |_: &Path, _: &Path| -> Result<(), String> {
        panic!("import must not run when the archive cannot be fetched")
    };
    let err = selfheal::tick(&cfg, &node, &import, &sink.log()).unwrap_err();
    assert!(err.contains("NOT started"), "{err}");
    assert!(node.alive(), "node left running");
    assert!(events.lock().unwrap().is_empty(), "not even stopped");
    assert_eq!(
        std::fs::read(base.join("chains/gpn_testnet/db/full/CURRENT")).unwrap(),
        b"FORKED-DB"
    );
}

#[test]
fn a_stop_that_fails_never_wipes_and_does_not_consume_the_window() {
    let tmp = Tmp::new("stuck");
    let base = forked_node_base(tmp.path());
    let archive = tmp.path().join("snap.tar.gz");
    std::fs::write(&archive, b"payload").unwrap();
    let (meta, pk) = signed_meta(
        tmp.path(),
        297_000,
        CANON,
        &archive,
        &archive.display().to_string(),
    );
    let sink = Sink::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    let node = FakeNode::spawn(events.clone(), Err("node did not stop".into()));
    let cfg = watch_cfg(&tmp, &base, rpc_stub(297_100, FORK), &meta, &pk);
    let import = |_: &Path, _: &Path| -> Result<(), String> { panic!("must not import") };
    let err = selfheal::tick(&cfg, &node, &import, &sink.log()).unwrap_err();
    assert!(err.contains("did not stop"), "{err}");
    assert_eq!(
        std::fs::read(base.join("chains/gpn_testnet/db/full/CURRENT")).unwrap(),
        b"FORKED-DB"
    );
    assert!(
        selfheal::read_recovery(&selfheal::ledger_path(&base))
            .unwrap()
            .is_none(),
        "a stop that never happened must not burn the 24 h window"
    );
    assert!(node.alive());
}

#[test]
fn a_ledger_that_already_healed_never_wipes_again() {
    let tmp = Tmp::new("ledger");
    let base = forked_node_base(tmp.path());
    let archive = tmp.path().join("snap.tar.gz");
    std::fs::write(&archive, b"payload").unwrap();
    let (meta, pk) = signed_meta(
        tmp.path(),
        297_000,
        CANON,
        &archive,
        &archive.display().to_string(),
    );
    let now = selfheal::now_unix();
    selfheal::write_recovery(
        &selfheal::ledger_path(&base),
        &selfheal::RecoveryRecord {
            recovered_at: now - 60,
            recovered_at_iso: selfheal::iso8601(now - 60),
            checkpoint_block: 297_000,
            checkpoint_hash: CANON.into(),
            local_hash: FORK.into(),
            fork_dir: base.join("chains-fork-1").display().to_string(),
            snapshot: archive.display().to_string(),
            completed: true,
            detail: "healed".into(),
        },
    )
    .unwrap();
    let sink = Sink::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    let node = FakeNode::spawn(events.clone(), Ok(()));
    let cfg = watch_cfg(&tmp, &base, rpc_stub(297_100, FORK), &meta, &pk);
    let import = |_: &Path, _: &Path| -> Result<(), String> { panic!("must not import twice") };
    assert!(!selfheal::tick(&cfg, &node, &import, &sink.log()).unwrap());
    assert!(node.alive());
    assert_eq!(
        std::fs::read(base.join("chains/gpn_testnet/db/full/CURRENT")).unwrap(),
        b"FORKED-DB"
    );
    assert!(
        sink.joined().contains("max one per 24 h"),
        "{}",
        sink.joined()
    );
}
