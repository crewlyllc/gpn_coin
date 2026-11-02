//! End-to-end tests for the `gpn_update` binary.
//!
//! Each test points the binary at a **local HTTP server** that serves a signed
//! manifest and a tiny bundle, and at throwaway state/install directories, so
//! the real download → verify → extract → flip → copy path runs without
//! touching the host or the network.
//!
//! The release private key is obviously not in the tree, so the binary accepts
//! `GPN_UPDATE_PUBKEY` **in debug builds only** (`cfg(debug_assertions)`); the
//! tests sign with a throwaway key and set it. Release builds ignore that
//! variable entirely and always trust `update::RELEASE_PUBKEY_HEX`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ed25519_dalek::Signer;
use gpn_agent::update::{self, ReleaseAsset, ReleaseManifest};
use tiny_http::{Header, Response, Server as HttpServer};

const BIN: &str = env!("CARGO_BIN_EXE_gpn_update");
const OLD_VERSION: &str = "0.3.11";
const NEW_VERSION: &str = "9.9.9";

// ─────────────────────────────────────────────────────────────────────────────
// Fixtures
// ─────────────────────────────────────────────────────────────────────────────

fn test_key() -> (ed25519_dalek::SigningKey, String) {
    let sk = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
    let pk = hex::encode(sk.verifying_key().to_bytes());
    (sk, pk)
}

fn td(tag: &str) -> PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let p = std::env::temp_dir().join(format!("gpn-update-cli-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn write_exe(dir: &Path, name: &str, version: Option<&str>) {
    std::fs::create_dir_all(dir).unwrap();
    let body = match version {
        Some(v) => {
            format!("#!/bin/sh\necho gpn-build-v1 version={v} sha=deadbeef spec=1\n# {name}\n")
        }
        None => format!("#!/bin/sh\necho {name}\n"),
    };
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// A tiny but structurally valid bundle: `gpn_node` with a build stamp,
/// `gpn-node`, and one more agent binary.
fn make_bundle(version: &str) -> Vec<u8> {
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    for (name, body) in [
        (
            "gpn_node",
            format!("#!/bin/sh\necho gpn-build-v1 version={version} sha=cafe spec=1\n")
                .into_bytes(),
        ),
        ("gpn-node", b"#!/bin/sh\necho rust-node\n".to_vec()),
        ("gpn_llm_provider", b"#!/bin/sh\necho provider\n".to_vec()),
    ] {
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o755);
        h.set_cksum();
        builder
            .append_data(&mut h, format!("gpn-linux/{name}"), body.as_slice())
            .unwrap();
    }
    let enc = builder.into_inner().unwrap();
    enc.finish().unwrap()
}

fn signed_manifest(
    version: &str,
    bundle: &[u8],
    sk: &ed25519_dalek::SigningKey,
    asset_url: &str,
) -> Vec<u8> {
    let asset = ReleaseAsset {
        os: update::platform_os().to_string(),
        arch: update::platform_arch().to_string(),
        url: asset_url.to_string(),
        sha256: update::sha256_hex(bundle),
        size: bundle.len() as u64,
        layout: update::LAYOUT_BUNDLE.to_string(),
        files: Default::default(),
    };
    let mut m = ReleaseManifest {
        version: version.to_string(),
        min_version: OLD_VERSION.to_string(),
        published_at: "2026-11-02T00:00:00Z".to_string(),
        assets: vec![asset],
        signing_key: String::new(),
        signature: String::new(),
    };
    m.signing_key = hex::encode(sk.verifying_key().to_bytes());
    let sig = sk.sign(&m.canonical_text());
    m.signature = hex::encode(sig.to_bytes());
    serde_json::to_vec_pretty(&m).unwrap()
}

// ─────────────────────────────────────────────────────────────────────────────
// Tiny HTTP server that records what was requested
// ─────────────────────────────────────────────────────────────────────────────

struct Feed {
    base: String,
    routes: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    hits: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Feed {
    fn start() -> Feed {
        let server = HttpServer::http("127.0.0.1:0").expect("bind tiny_http");
        let base = format!("http://{}", server.server_addr());
        let hits = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let routes: Arc<Mutex<HashMap<String, Vec<u8>>>> = Arc::new(Mutex::new(HashMap::new()));
        let (h, s, r) = (hits.clone(), stop.clone(), routes.clone());
        let join = std::thread::spawn(move || {
            while !s.load(Ordering::SeqCst) {
                let Ok(Some(req)) = server.recv_timeout(Duration::from_millis(50)) else {
                    continue;
                };
                let url = req.url().to_string();
                h.lock().unwrap().push(url.clone());
                let body = r.lock().unwrap().get(&url).cloned();
                match body {
                    Some(body) => {
                        let hdr =
                            Header::from_bytes("Content-Type", "application/octet-stream").unwrap();
                        let _ = req.respond(Response::from_data(body).with_header(hdr));
                    }
                    None => {
                        let _ = req.respond(Response::empty(404));
                    }
                }
            }
        });
        Feed {
            base,
            routes,
            hits,
            stop,
            join: Some(join),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    /// Publish (or replace) a route. The manifest needs absolute asset URLs,
    /// i.e. the server's port, so routes are added after `start`.
    fn add(&self, path: &str, body: Vec<u8>) {
        self.routes.lock().unwrap().insert(path.to_string(), body);
    }

    fn hits(&self) -> Vec<String> {
        self.hits.lock().unwrap().clone()
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Running the binary
// ─────────────────────────────────────────────────────────────────────────────

struct Ctx {
    root: PathBuf,
    state: PathBuf,
    target: PathBuf,
}

impl Ctx {
    fn new(tag: &str) -> Ctx {
        let root = td(tag);
        let state = root.join("update");
        let target = root.join("bin");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        Ctx {
            root,
            state,
            target,
        }
    }

    /// Seed the install dir with the version the host is running.
    fn seed_old(&self) {
        write_exe(&self.target, "gpn_node", Some(OLD_VERSION));
        write_exe(&self.target, "gpn-node", None);
    }

    fn run(&self, args: &[&str], extra_env: &[(&str, &str)], url: &str) -> (i32, String, String) {
        let mut cmd = std::process::Command::new(BIN);
        cmd.args(args)
            .env("GPN_UPDATE_URL", url)
            .env("GPN_UPDATE_PUBKEY", test_key().1)
            .env("GPN_UPDATE_DIR", &self.state)
            .env("GPN_UPDATE_TARGET_DIR", &self.target)
            .env("HOME", &self.root)
            // Never inherit a policy/restart setting from the developer's shell.
            .env_remove("GPN_AUTO_UPDATE")
            .env_remove("GPN_UPDATE_RESTART_CMD")
            .env_remove("GPN_PREFIX")
            .env_remove("GPN_UPDATE_TEST_FAIL_AFTER_COPIES");
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("run gpn_update");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// --check
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn check_newer_exits_3_and_downloads_nothing() {
    let ctx = Ctx::new("check-newer");
    ctx.seed_old();
    let (sk, _) = test_key();
    let bundle = make_bundle(NEW_VERSION);
    let feed = Feed::start();
    let asset_url = feed.url("/gpn-linux.tar.gz");
    feed.add(
        "/releases.json",
        signed_manifest(NEW_VERSION, &bundle, &sk, &asset_url),
    );
    feed.add("/gpn-linux.tar.gz", bundle);

    let (code, out, err) = ctx.run(&["--check", "--json"], &[], &feed.url("/releases.json"));
    assert_eq!(code, 3, "stdout={out} stderr={err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("--json output parses");
    assert_eq!(v["current"], OLD_VERSION);
    assert_eq!(v["latest"], NEW_VERSION);
    assert_eq!(v["update_available"], true);
    assert_eq!(
        feed.hits(),
        vec!["/releases.json"],
        "--check must never download the asset"
    );
}

#[test]
fn check_same_version_exits_0() {
    let ctx = Ctx::new("check-same");
    ctx.seed_old();
    let (sk, _) = test_key();
    let bundle = make_bundle(OLD_VERSION);
    let feed = Feed::start();
    let asset_url = feed.url("/gpn-linux.tar.gz");
    feed.add(
        "/releases.json",
        signed_manifest(OLD_VERSION, &bundle, &sk, &asset_url),
    );

    let (code, out, err) = ctx.run(&["--check"], &[], &feed.url("/releases.json"));
    assert_eq!(code, 0, "stdout={out} stderr={err}");
    assert!(out.contains("up to date"), "stdout={out}");
    assert_eq!(feed.hits(), vec!["/releases.json"]);
}

#[test]
fn check_malformed_manifest_exits_2() {
    let ctx = Ctx::new("check-malformed");
    ctx.seed_old();
    let feed = Feed::start();
    feed.add("/releases.json", b"<html>captive portal</html>".to_vec());

    let (code, _out, err) = ctx.run(&["--check"], &[], &feed.url("/releases.json"));
    assert_eq!(code, 2, "stderr={err}");
}

#[test]
fn check_unreachable_exits_2() {
    let ctx = Ctx::new("check-unreachable");
    ctx.seed_old();
    // Nothing is listening on port 1.
    let (code, _out, err) = ctx.run(&["--check"], &[], "http://127.0.0.1:1/releases.json");
    assert_eq!(code, 2, "stderr={err}");
}

#[test]
fn check_bad_signature_exits_1_and_downloads_nothing() {
    let ctx = Ctx::new("check-badsig");
    ctx.seed_old();
    // Signed by a key the binary does not trust.
    let other = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
    let bundle = make_bundle(NEW_VERSION);
    let feed = Feed::start();
    let asset_url = feed.url("/gpn-linux.tar.gz");
    feed.add(
        "/releases.json",
        signed_manifest(NEW_VERSION, &bundle, &other, &asset_url),
    );
    feed.add("/gpn-linux.tar.gz", bundle);

    let (code, _out, err) = ctx.run(&["--check"], &[], &feed.url("/releases.json"));
    assert_eq!(code, 1, "stderr={err}");
    assert!(
        err.contains("rejected") || err.contains("key"),
        "stderr={err}"
    );
    assert_eq!(
        feed.hits(),
        vec!["/releases.json"],
        "a rejected manifest must not cause a download"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// --apply
// ─────────────────────────────────────────────────────────────────────────────

fn apply_feed() -> (Feed, Vec<u8>) {
    let (sk, _) = test_key();
    let bundle = make_bundle(NEW_VERSION);
    let feed = Feed::start();
    let asset_url = feed.url("/gpn-linux.tar.gz");
    feed.add(
        "/releases.json",
        signed_manifest(NEW_VERSION, &bundle, &sk, &asset_url),
    );
    feed.add("/gpn-linux.tar.gz", bundle.clone());
    (feed, bundle)
}

#[test]
fn apply_installs_bundle_flips_current_and_prunes() {
    let ctx = Ctx::new("apply-ok");
    ctx.seed_old();
    // An older version directory that must be pruned after the flip.
    let stale = update::version_dir(&ctx.state, "1.0.0");
    write_exe(&stale, update::NODE_BIN, Some("1.0.0"));
    write_exe(&stale, "gpn-node", None);
    // And the version the host is running.
    let old = update::version_dir(&ctx.state, OLD_VERSION);
    write_exe(&old, update::NODE_BIN, Some(OLD_VERSION));
    write_exe(&old, "gpn-node", None);

    let (feed, _bundle) = apply_feed();
    let (code, out, err) = ctx.run(&["--apply", "--json"], &[], &feed.url("/releases.json"));
    assert_eq!(code, 0, "stdout={out} stderr={err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["applied"], true);

    // 1. The install dir has the new binaries, executable.
    let supervisor = std::fs::read_to_string(ctx.target.join("gpn_node")).unwrap();
    assert!(
        supervisor.contains(&format!("gpn-build-v1 version={NEW_VERSION}")),
        "{supervisor}"
    );
    let rust = std::fs::read_to_string(ctx.target.join("gpn-node")).unwrap();
    assert!(rust.contains("rust-node"), "{rust}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(ctx.target.join("gpn_node"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "installed binary must be executable");
    }

    // 2. `current` points at the new version tree.
    let cur = update::current_version_dir(&ctx.state).expect("current exists");
    assert_eq!(cur, update::version_dir(&ctx.state, NEW_VERSION));
    assert_eq!(
        update::installed_version(&ctx.state).as_deref(),
        Some(NEW_VERSION)
    );

    // 3. The previous version is kept and recorded.
    let prev = update::previous_pointer_label(&ctx.state).expect("previous pointer");
    assert_eq!(prev, OLD_VERSION);
    assert!(update::version_dir(&ctx.state, OLD_VERSION).is_dir());

    // 4. Anything older than current/previous is pruned.
    assert!(
        !stale.exists(),
        "1.0.0 should have been pruned: {} still exists",
        stale.display()
    );

    // 5. The whole bundle was downloaded exactly once.
    assert_eq!(
        feed.hits(),
        vec!["/releases.json", "/gpn-linux.tar.gz"],
        "one manifest fetch and one download"
    );
}

#[test]
fn apply_mid_copy_failure_leaves_the_old_supervisor_intact() {
    let ctx = Ctx::new("apply-crash");
    ctx.seed_old();
    let before = std::fs::read(ctx.target.join("gpn_node")).unwrap();

    let (feed, _) = apply_feed();
    // Sorted copy order is gpn-node, gpn-node…: `gpn-node` (< '_') is first, so
    // failing after one copy proves the supervisor was never touched.
    let (code, out, err) = ctx.run(
        &["--apply", "--quiet"],
        &[("GPN_UPDATE_TEST_FAIL_AFTER_COPIES", "1")],
        &feed.url("/releases.json"),
    );
    assert_eq!(code, 1, "stdout={out} stderr={err}");

    let after = std::fs::read(ctx.target.join("gpn_node")).unwrap();
    assert_eq!(
        after, before,
        "a crash between per-file swaps must leave the old supervisor byte-identical"
    );
    // No half-written temp file is left behind.
    let leftovers: Vec<String> = std::fs::read_dir(&ctx.target)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp-"))
        .collect();
    assert!(leftovers.is_empty(), "leftover temp files: {leftovers:?}");

    // Re-running is idempotent and reuses the already-verified tree (no second
    // download) instead of pulling the bundle again.
    let (code2, out2, err2) = ctx.run(&["--apply", "--quiet"], &[], &feed.url("/releases.json"));
    assert_eq!(code2, 0, "stdout={out2} stderr={err2}");
    let healed = std::fs::read_to_string(ctx.target.join("gpn_node")).unwrap();
    assert!(
        healed.contains(&format!("gpn-build-v1 version={NEW_VERSION}")),
        "{healed}"
    );
    assert_eq!(
        feed.hits(),
        vec!["/releases.json", "/gpn-linux.tar.gz", "/releases.json"],
        "the retry must not download the bundle twice"
    );
}

#[test]
fn apply_refuses_when_auto_update_disabled() {
    let ctx = Ctx::new("apply-disabled");
    ctx.seed_old();
    let before = std::fs::read(ctx.target.join("gpn_node")).unwrap();

    let (feed, _) = apply_feed();
    let (code, _out, err) = ctx.run(
        &["--apply"],
        &[("GPN_AUTO_UPDATE", "0")],
        &feed.url("/releases.json"),
    );
    assert_eq!(code, 3, "stderr={err}");
    assert!(err.contains("GPN_AUTO_UPDATE=0"), "stderr={err}");
    assert_eq!(
        feed.hits(),
        vec!["/releases.json"],
        "no download when refused"
    );
    assert_eq!(std::fs::read(ctx.target.join("gpn_node")).unwrap(), before);
    assert!(update::current_version_dir(&ctx.state).is_none());
}

#[test]
fn apply_up_to_date_is_a_noop() {
    let ctx = Ctx::new("apply-noop");
    ctx.seed_old();
    let (sk, _) = test_key();
    let bundle = make_bundle(OLD_VERSION);
    let feed = Feed::start();
    let asset_url = feed.url("/gpn-linux.tar.gz");
    feed.add(
        "/releases.json",
        signed_manifest(OLD_VERSION, &bundle, &sk, &asset_url),
    );

    let before = std::fs::read(ctx.target.join("gpn_node")).unwrap();
    let (code, out, err) = ctx.run(&["--apply"], &[], &feed.url("/releases.json"));
    assert_eq!(code, 0, "stdout={out} stderr={err}");
    assert_eq!(feed.hits(), vec!["/releases.json"]);
    assert_eq!(std::fs::read(ctx.target.join("gpn_node")).unwrap(), before);
}
