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

// These sign manifests with a THROWAWAY key and rely on `GPN_UPDATE_PUBKEY`,
// which the binary honours in debug builds only (release always trusts the
// compiled-in release key). Running them under `--release` therefore fails by
// design -- CI runs them in debug (see the dedicated step in ci.yml).
#![cfg(debug_assertions)]

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
            format!("#!/bin/sh\necho gpn-build-v1 version={version} sha=cafe1234 spec=1\n")
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
    /// `path -> (status, remaining failures)`: serve that status instead of the
    /// body until the count runs out. Used to prove the download path rides out
    /// a transient 5xx and still fails closed on a 4xx.
    faults: Arc<Mutex<HashMap<String, (u16, usize)>>>,
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
        let faults: Arc<Mutex<HashMap<String, (u16, usize)>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (h, s, r, f) = (hits.clone(), stop.clone(), routes.clone(), faults.clone());
        let join = std::thread::spawn(move || {
            while !s.load(Ordering::SeqCst) {
                let Ok(Some(req)) = server.recv_timeout(Duration::from_millis(50)) else {
                    continue;
                };
                let url = req.url().to_string();
                h.lock().unwrap().push(url.clone());
                // Fault injection first: a route can be told to answer N times
                // with a status before it starts serving the body.
                let fault = {
                    let mut g = f.lock().unwrap();
                    match g.get_mut(&url) {
                        Some((status, remaining)) if *remaining > 0 => {
                            *remaining -= 1;
                            Some(*status)
                        }
                        _ => None,
                    }
                };
                if let Some(status) = fault {
                    let _ = req.respond(Response::empty(status));
                    continue;
                }
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
            faults,
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

    /// Answer the next `times` requests for `path` with `status`, then serve the
    /// body as normal.
    fn fail_first(&self, path: &str, status: u16, times: usize) {
        self.faults
            .lock()
            .unwrap()
            .insert(path.to_string(), (status, times));
    }

    fn hits(&self) -> Vec<String> {
        self.hits.lock().unwrap().clone()
    }

    fn hit_count(&self, path: &str) -> usize {
        self.hits().iter().filter(|h| *h == path).count()
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
        self.run_inner(args, extra_env, url, true)
    }

    /// Same, but the port pre-flight is *not* told that the test process holds
    /// the RPC port: the fixture is then a genuinely foreign holder, which is
    /// what the refusal branch must be tested against.
    fn run_undeclared(
        &self,
        args: &[&str],
        extra_env: &[(&str, &str)],
        url: &str,
    ) -> (i32, String, String) {
        self.run_inner(args, extra_env, url, false)
    }

    fn run_inner(
        &self,
        args: &[&str],
        extra_env: &[(&str, &str)],
        url: &str,
        declare_test_node: bool,
    ) -> (i32, String, String) {
        // The canary proof lives beside the manifest on the same fixture server.
        let canary_url = url
            .rsplit_once('/')
            .map(|(base, _)| format!("{base}/canary.json"))
            .unwrap_or_else(|| url.to_string());
        let mut cmd = std::process::Command::new(BIN);
        cmd.args(args)
            .env("GPN_UPDATE_URL", url)
            .env("GPN_CANARY_URL", &canary_url)
            .env("GPN_UPDATE_PUBKEY", test_key().1)
            .env("GPN_UPDATE_DIR", &self.state)
            .env("GPN_UPDATE_TARGET_DIR", &self.target)
            .env("HOME", &self.root)
            // The fake node is a `tiny_http` server inside this test process,
            // so it holds the RPC port under this pid. Debug builds let a test
            // declare exactly that pid; release builds ignore the variable.
            .env(
                "GPN_UPDATE_PREFLIGHT_ALLOW_PID",
                if declare_test_node {
                    std::process::id().to_string()
                } else {
                    String::new()
                },
            )
            // Never inherit a policy/restart setting from the developer's shell.
            .env_remove("GPN_AUTO_UPDATE")
            .env_remove("GPN_CANARY")
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

/// A published canary proof for `version` that a non-canary node may act on
/// (row 275: the gate refuses to install anything a canary has not proven).
fn canary_proof(version: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "version": version,
        "ok": true,
        "job_served": true,
        "confirmed_at": 1_760_000_000u64,
        "failed_at": 0u64,
        "reason": "",
    }))
    .unwrap()
}

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
    // `--apply` is gated on a published canary proof; every apply fixture
    // starts from the state a healthy fleet is in once one node has proven the
    // release. The hold itself is tested separately, below.
    feed.add("/canary.json", canary_proof(NEW_VERSION));
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

    // 5. The whole bundle was downloaded exactly once. The canary proof is
    //    fetched between the manifest and the asset (row 275's gate).
    assert_eq!(
        feed.hits(),
        vec!["/releases.json", "/canary.json", "/gpn-linux.tar.gz"],
        "one manifest fetch, one canary check and one download"
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
        vec![
            "/releases.json",
            "/canary.json",
            "/gpn-linux.tar.gz",
            "/releases.json",
            "/canary.json"
        ],
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

// ─────────────────────────────────────────────────────────────────────────────
// #148: the signed release must not overwrite a locally deployed build
// ─────────────────────────────────────────────────────────────────────────────

/// The reported incident, exactly: `gpn-update.timer` replaced a locally
/// deployed `HEAD` build because the *supervisor* in the install dir was still
/// the previous release. A version comparison sees "0.3.x → 0.3.x+1, go ahead";
/// only the commit in the stamp shows that one of the binaries is newer code
/// than the release contains.
#[test]
fn apply_refuses_to_replace_a_local_build_of_the_release_version() {
    let ctx = Ctx::new("apply-local-head");
    // Mixed install dir: supervisor = old release, scheduler = a local HEAD
    // build of the *candidate's* version (bundle stamps sha=cafe, this deadbeef).
    ctx.seed_old();
    write_exe(&ctx.target, "gpn_scheduler", Some(NEW_VERSION));
    let local_before = std::fs::read(ctx.target.join("gpn_scheduler")).unwrap();
    let supervisor_before = std::fs::read(ctx.target.join("gpn_node")).unwrap();

    let (feed, _bundle) = apply_feed();
    let (code, out, err) = ctx.run(&["--apply", "--json"], &[], &feed.url("/releases.json"));
    assert_eq!(
        code, 3,
        "a refusal is not an error: stdout={out} stderr={err}"
    );
    assert!(err.contains("gpn_scheduler"), "names the file: {err}");
    assert!(err.contains("deadbeef"), "names the installed build: {err}");
    assert!(err.contains("cafe1234"), "names the release build: {err}");
    assert!(err.contains("--force"), "names the escape hatch: {err}");

    // Nothing was touched: not the binaries, not `current`.
    assert_eq!(
        std::fs::read(ctx.target.join("gpn_scheduler")).unwrap(),
        local_before
    );
    assert_eq!(
        std::fs::read(ctx.target.join("gpn_node")).unwrap(),
        supervisor_before
    );
    assert!(
        update::current_version_dir(&ctx.state).is_none(),
        "a refused install must not flip `current`"
    );

    // The operator can say yes on purpose.
    let (code, out, err) = ctx.run(&["--apply", "--force"], &[], &feed.url("/releases.json"));
    assert_eq!(code, 0, "stdout={out} stderr={err}");
    let supervisor = std::fs::read_to_string(ctx.target.join("gpn_node")).unwrap();
    assert!(supervisor.contains(&format!("gpn-build-v1 version={NEW_VERSION}")));
    assert_eq!(
        update::installed_version(&ctx.state).as_deref(),
        Some(NEW_VERSION)
    );
}

/// A downgrade of any file in the install dir is refused, even when the
/// supervisor is older than the release (the mixed-dir shape again).
#[test]
fn apply_refuses_a_downgrade() {
    let ctx = Ctx::new("apply-downgrade");
    ctx.seed_old();
    // One binary in the install dir is already newer than anything published.
    write_exe(&ctx.target, "gpn_scheduler", Some("10.0.0"));

    let (feed, _bundle) = apply_feed();
    let (code, out, err) = ctx.run(&["--apply"], &[], &feed.url("/releases.json"));
    assert_eq!(code, 3, "stdout={out} stderr={err}");
    assert!(err.contains("downgrade"), "stderr={err}");
    assert!(err.contains("gpn_scheduler"), "names the file: {err}");
    assert!(update::current_version_dir(&ctx.state).is_none());
}

// -----------------------------------------------------------------------------
// Canary gate (row 275)
// -----------------------------------------------------------------------------

// `--apply` is gated on a published proof that one node already ran the
// release and served a job on it. These three tests pin the gate from the
// binary's side: held without a proof, held on a proof for another version,
// and the canary itself installing first.

/// No published proof (the fleet's state before any node has run the release)
/// => the node stays on its current version and says why, exit 3.
#[test]
fn apply_holds_when_no_canary_proof_is_published() {
    let ctx = Ctx::new("apply-canary-missing");
    ctx.seed_old();
    let (feed, _bundle) = apply_feed();
    let (code, out, err) = ctx.run(
        &["--apply"],
        // Nothing listens on port 1: the proof is unreadable, not absent-but-ok.
        &[("GPN_CANARY_URL", "http://127.0.0.1:1/canary.json")],
        &feed.url("/releases.json"),
    );
    assert_eq!(code, 3, "a hold is not an error: stdout={out} stderr={err}");
    assert!(
        err.contains("not the canary") && err.contains("no canary proof published for"),
        "stderr must say the node is held for want of a proof: {err}"
    );
    assert_eq!(feed.hit_count("/gpn-linux.tar.gz"), 0, "nothing downloaded");
    assert!(
        update::current_version_dir(&ctx.state).is_none(),
        "a held node must not flip `current`"
    );
}

/// A proof for a *different* version is no proof: the fleet stays put.
#[test]
fn apply_holds_when_the_canary_proof_is_for_another_version() {
    let ctx = Ctx::new("apply-canary-other");
    ctx.seed_old();
    let (feed, _bundle) = apply_feed();
    feed.add("/canary.json", canary_proof("8.8.8"));
    let (code, out, err) = ctx.run(&["--apply"], &[], &feed.url("/releases.json"));
    assert_eq!(code, 3, "stdout={out} stderr={err}");
    assert!(
        err.contains("is for 8.8.8 not 9.9.9"),
        "stderr must name both versions: {err}"
    );
    assert_eq!(feed.hit_count("/gpn-linux.tar.gz"), 0, "nothing downloaded");
    assert!(update::current_version_dir(&ctx.state).is_none());
}

/// The canary itself installs first, before any proof exists - that is how the
/// first proof ever gets produced.
#[test]
fn apply_canary_installs_without_waiting_for_a_proof() {
    let ctx = Ctx::new("apply-canary-self");
    ctx.seed_old();
    let (feed, _bundle) = apply_feed();
    let (code, out, err) = ctx.run(
        &["--apply"],
        &[
            ("GPN_CANARY", "1"),
            ("GPN_CANARY_URL", "http://127.0.0.1:1/canary.json"),
        ],
        &feed.url("/releases.json"),
    );
    assert_eq!(code, 0, "stdout={out} stderr={err}");
    assert!(
        out.contains("canary: this node installs 9.9.9 first"),
        "stdout must say the node is the canary: {out}"
    );
    let supervisor = std::fs::read_to_string(ctx.target.join("gpn_node")).unwrap();
    assert!(
        supervisor.contains(&format!("gpn-build-v1 version={NEW_VERSION}")),
        "{supervisor}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Transient mirror failures (#150)
// ─────────────────────────────────────────────────────────────────────────────

/// The 2026-11-08 00:55 PT fire: one `HTTP 504` from the release mirror burned a
/// whole six-hour update cycle. Two 504s then a 200 must still install.
#[test]
fn apply_rides_out_transient_5xx_and_still_installs() {
    let ctx = Ctx::new("apply-504-twice");
    ctx.seed_old();
    let (feed, _bundle) = apply_feed();
    feed.fail_first("/gpn-linux.tar.gz", 504, 2);

    let (code, out, err) = ctx.run(
        &["--apply", "--json"],
        &[("GPN_UPDATE_RETRY_BASE_MS", "5")],
        &feed.url("/releases.json"),
    );
    assert_eq!(code, 0, "stdout={out} stderr={err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["applied"], true);

    // Three attempts against the asset: 504, 504, then the bundle.
    assert_eq!(
        feed.hit_count("/gpn-linux.tar.gz"),
        3,
        "hits={:?}",
        feed.hits()
    );
    let supervisor = std::fs::read_to_string(ctx.target.join("gpn_node")).unwrap();
    assert!(
        supervisor.contains(&format!("gpn-build-v1 version={NEW_VERSION}")),
        "{supervisor}"
    );
    assert_eq!(
        update::installed_version(&ctx.state).as_deref(),
        Some(NEW_VERSION)
    );
    assert!(
        err.contains("succeeded on attempt 3/3"),
        "the retry must be visible in the journal: {err}"
    );
}

/// A 4xx is the mirror saying the request is wrong — one attempt, fail closed,
/// install dir untouched.
#[test]
fn apply_does_not_retry_a_404_on_the_asset() {
    let ctx = Ctx::new("apply-404");
    ctx.seed_old();
    let (feed, _bundle) = apply_feed();
    // Serve the bundle, but as an unavailable asset: a permanent answer.
    feed.fail_first("/gpn-linux.tar.gz", 404, usize::MAX);

    let (code, out, err) = ctx.run(
        &["--apply"],
        &[("GPN_UPDATE_RETRY_BASE_MS", "5")],
        &feed.url("/releases.json"),
    );
    assert_ne!(code, 0, "stdout={out} stderr={err}");
    assert_eq!(
        feed.hit_count("/gpn-linux.tar.gz"),
        1,
        "a 404 must be attempted exactly once: hits={:?}",
        feed.hits()
    );
    assert!(err.contains("404"), "stderr={err}");
    assert!(
        update::current_version_dir(&ctx.state).is_none(),
        "a failed download must not flip `current`"
    );
    // The install dir keeps the old supervisor.
    let supervisor = std::fs::read_to_string(ctx.target.join("gpn_node")).unwrap();
    assert!(supervisor.contains(&format!("gpn-build-v1 version={OLD_VERSION}")));
}

/// A mirror that is down for every attempt gives up in bounded time and names
/// the attempt count, instead of hanging the systemd timer.
#[test]
fn apply_gives_up_after_the_attempt_budget() {
    let ctx = Ctx::new("apply-503-forever");
    ctx.seed_old();
    let (feed, _bundle) = apply_feed();
    feed.fail_first("/gpn-linux.tar.gz", 503, usize::MAX);

    let (code, out, err) = ctx.run(
        &["--apply"],
        &[("GPN_UPDATE_RETRY_BASE_MS", "5")],
        &feed.url("/releases.json"),
    );
    assert_ne!(code, 0, "stdout={out} stderr={err}");
    assert_eq!(
        feed.hit_count("/gpn-linux.tar.gz"),
        3,
        "exactly three attempts: hits={:?}",
        feed.hits()
    );
    assert!(err.contains("after 3 attempts"), "stderr={err}");
    assert!(update::current_version_dir(&ctx.state).is_none());
}

// ─────────────────────────────────────────────────────────────────────────────
// Confirmation and rollback: "installed" is not "running"
// ─────────────────────────────────────────────────────────────────────────────

/// A JSON-RPC answer from the node the updater just restarted.
fn rpc_body(version: &str) -> Vec<u8> {
    serde_json::json!({ "jsonrpc": "2.0", "id": 1, "result": version })
        .to_string()
        .into_bytes()
}

#[test]
fn confirm_sees_the_version_that_is_actually_running() {
    let ctx = Ctx::new("confirm-ok");
    let feed = Feed::start();
    feed.add("/rpc", rpc_body(NEW_VERSION));
    let rpc = feed.url("/rpc");

    let (code, out, err) = ctx.run(
        &[
            "--confirm",
            "--expect",
            NEW_VERSION,
            "--timeout",
            "5",
            "--rpc",
            &rpc,
        ],
        &[],
        "http://127.0.0.1:1/releases.json",
    );
    assert_eq!(code, 0, "stdout={out} stderr={err}");
    assert!(out.contains("confirmed"), "stdout={out}");
}

/// The box-3 shape: the tree is installed, the OLD build is still the one
/// answering. That must be a failure, not an ambiguity.
#[test]
fn confirm_fails_when_the_old_version_is_still_running() {
    let ctx = Ctx::new("confirm-stale");
    let feed = Feed::start();
    feed.add("/rpc", rpc_body(OLD_VERSION));
    let rpc = feed.url("/rpc");

    let (code, out, err) = ctx.run(
        &[
            "--confirm",
            "--expect",
            NEW_VERSION,
            "--timeout",
            "2",
            "--rpc",
            &rpc,
        ],
        &[],
        "http://127.0.0.1:1/releases.json",
    );
    assert_eq!(code, 4, "stdout={out} stderr={err}");
    assert!(err.contains(NEW_VERSION), "stderr={err}");
    assert!(err.contains(OLD_VERSION), "names what it saw: {err}");
    // The message names the failing signal, not a guess about the process.
    assert!(err.contains("signal=rpc"), "stderr={err}");
    assert!(err.contains("not confirmed"), "stderr={err}");
}

/// The port pre-flight must still refuse a holder nobody vouched for: the
/// debug-only allowlist is per-pid, not a blanket off-switch.
#[test]
fn the_port_preflight_still_refuses_an_undeclared_holder() {
    let ctx = Ctx::new("preflight-foreign");
    ctx.seed_old();
    let (feed, _bundle) = apply_feed();
    feed.add("/rpc", rpc_body(NEW_VERSION));
    let rpc = feed.url("/rpc");

    let (code, _out, err) = ctx.run_undeclared(
        &["--apply", "--json", "--timeout", "2"],
        &[
            ("GPN_UPDATE_RESTART_CMD", "true"),
            ("GPN_BOOT_ID", "boot-foreign"),
            ("GPN_RPC_URL", &rpc),
        ],
        &feed.url("/releases.json"),
    );
    assert_ne!(code, 0, "a foreign port holder is a failure: {err}");
    assert!(
        err.contains("not a gpn process"),
        "the refusal names the holder: {err}"
    );
    assert!(err.contains("not touching it"), "{err}");
}

/// Nothing listening at all is the same answer: not confirmed, in bounded time.
#[test]
fn confirm_is_bounded_when_nothing_is_listening() {
    let ctx = Ctx::new("confirm-down");
    let started = std::time::Instant::now();
    let (code, _out, err) = ctx.run(
        &[
            "--confirm",
            "--expect",
            NEW_VERSION,
            "--timeout",
            "1",
            "--rpc",
            "http://127.0.0.1:1/rpc",
        ],
        &[],
        "http://127.0.0.1:1/releases.json",
    );
    assert_eq!(code, 4, "stderr={err}");
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "must give up in bounded time, took {:?}",
        started.elapsed()
    );
}

#[test]
fn confirm_requires_an_expected_version() {
    let ctx = Ctx::new("confirm-noexpect");
    let (code, _out, err) = ctx.run(&["--confirm"], &[], "http://127.0.0.1:1/releases.json");
    assert_eq!(code, 1, "stderr={err}");
    assert!(err.contains("--expect"), "stderr={err}");
}

/// `--rollback` is one link flip: `current` goes back to `current.previous`.
#[test]
fn rollback_flips_current_back_one_version() {
    let ctx = Ctx::new("rollback");
    for v in [OLD_VERSION, NEW_VERSION] {
        write_exe(
            &update::version_dir(&ctx.state, v),
            update::NODE_BIN,
            Some(v),
        );
        write_exe(&update::version_dir(&ctx.state, v), "gpn-node", None);
    }
    update::record_previous(&ctx.state, &update::version_dir(&ctx.state, OLD_VERSION)).unwrap();
    update::activate_version(&ctx.state, NEW_VERSION).unwrap();
    assert_eq!(
        update::installed_version(&ctx.state).as_deref(),
        Some(NEW_VERSION)
    );

    let (code, out, err) = ctx.run(&["--rollback"], &[], "http://127.0.0.1:1/releases.json");
    assert_eq!(code, 0, "stdout={out} stderr={err}");
    assert_eq!(
        update::installed_version(&ctx.state).as_deref(),
        Some(OLD_VERSION),
        "current must point at the previous tree again"
    );
    // One step means one step: the forward direction is now the rollback target.
    assert_eq!(
        update::previous_pointer_label(&ctx.state).as_deref(),
        Some(NEW_VERSION)
    );
    // Both trees survive — nothing was deleted to make the flip work.
    assert!(update::node_bin(&update::version_dir(&ctx.state, NEW_VERSION)).is_file());
}

#[test]
fn rollback_without_a_target_fails_without_touching_anything() {
    let ctx = Ctx::new("rollback-none");
    let (code, _out, err) = ctx.run(&["--rollback"], &[], "http://127.0.0.1:1/releases.json");
    assert_eq!(code, 1, "stderr={err}");
    assert!(err.contains("no rollback target"), "stderr={err}");
}

// ─────────────────────────────────────────────────────────────────────────────
// The whole loop: apply → restart → confirm → (rollback)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn apply_restarts_confirms_and_records_the_winning_version() {
    let ctx = Ctx::new("loop-ok");
    ctx.seed_old();
    let (feed, _bundle) = apply_feed();
    feed.add("/rpc", rpc_body(NEW_VERSION));
    let rpc = feed.url("/rpc");

    let (code, out, err) = ctx.run(
        &["--apply", "--json"],
        &[
            ("GPN_UPDATE_RESTART_CMD", "true"),
            ("GPN_BOOT_ID", "boot-ok"),
            ("GPN_RPC_URL", &rpc),
        ],
        &feed.url("/releases.json"),
    );
    assert_eq!(code, 0, "stdout={out} stderr={err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["applied"], true);
    assert_eq!(v["confirmed_version"], NEW_VERSION);
    assert_eq!(
        update::installed_version(&ctx.state).as_deref(),
        Some(NEW_VERSION)
    );

    // And it is the running build that was asked, not the file on disk.
    assert_eq!(feed.hit_count("/rpc"), 1, "hits={:?}", feed.hits());
    let ledger = update::load_attempts(&ctx.state);
    let last = ledger.attempts.last().expect("an attempt is recorded");
    assert_eq!(last.version, NEW_VERSION);
    assert_eq!(last.boot_id, "boot-ok");
    assert_eq!(last.outcome, update::OUTCOME_CONFIRMED);
}

/// The requirement in one test: if the new build does not report itself after
/// the restart, `current` goes back, the old build is started again, and the
/// machine is left running — not dark.
#[test]
fn a_release_that_does_not_come_up_is_rolled_back_and_restarted() {
    let ctx = Ctx::new("loop-rollback");
    ctx.seed_old();
    let (feed, _bundle) = apply_feed();
    // The node keeps answering the OLD version: the new build did not take.
    feed.add("/rpc", rpc_body(OLD_VERSION));
    let rpc = feed.url("/rpc");

    let (code, out, err) = ctx.run(
        &["--apply", "--json", "--timeout", "2"],
        &[
            ("GPN_UPDATE_RESTART_CMD", "true"),
            ("GPN_BOOT_ID", "boot-bad"),
            ("GPN_RPC_URL", &rpc),
        ],
        &feed.url("/releases.json"),
    );
    assert_eq!(code, 1, "stdout={out} stderr={err}");
    assert!(err.contains("did not come up"), "stderr={err}");
    assert!(err.contains("rolled"), "stderr={err}");

    // 1. `current` is back on the previous version.
    assert_eq!(
        update::installed_version(&ctx.state).as_deref(),
        Some(OLD_VERSION)
    );
    // 2. The previous tree is intact — the rollback did not delete it.
    assert!(update::node_bin(&update::version_dir(&ctx.state, OLD_VERSION)).is_file());
    // 3. The restart ran a second time (once for the new build, once after the
    //    rollback), so the node is up on *something*.
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["restart_ran"], true);
    assert_eq!(v["rolled_back_to"], OLD_VERSION);
    // 4. The failure is on the record: this boot must not retry it.
    let ledger = update::load_attempts(&ctx.state);
    let last = ledger.attempts.last().unwrap();
    assert_eq!(last.outcome, update::OUTCOME_ROLLED_BACK);
    assert!(update::attempt_blocks(last));
}

/// #203: a failed release is retried with backoff, and it never stops trying.
/// The old behaviour — refuse for the rest of the boot — is exactly what left
/// box 1 two releases behind on 2026-11-16, so it is now a test that fails if
/// anyone reintroduces a permanent stop.
#[test]
fn a_failed_release_is_retried_after_its_backoff_and_never_permanently() {
    let ctx = Ctx::new("loop-once");
    ctx.seed_old();
    let (feed, _bundle) = apply_feed();
    feed.add("/rpc", rpc_body(OLD_VERSION));
    let rpc = feed.url("/rpc");
    let url = feed.url("/releases.json");

    let env = [
        ("GPN_UPDATE_RESTART_CMD", "true"),
        ("GPN_BOOT_ID", "boot-once"),
        ("GPN_RPC_URL", rpc.as_str()),
    ];
    let (code, _out, err) = ctx.run(&["--apply", "--timeout", "1"], &env, &url);
    assert_eq!(code, 1, "first attempt fails: {err}");
    let downloads_after_first = feed.hit_count("/gpn-linux.tar.gz");

    // 1. The failure set a retry clock rather than a permanent stop.
    let rec = update::attempt_for_version(&ctx.state, NEW_VERSION).expect("recorded");
    assert_eq!(rec.outcome, update::OUTCOME_ROLLED_BACK);
    assert_eq!(rec.attempts, 1, "first attempt counts as one");
    assert!(
        rec.next_retry_at > update::now_unix(),
        "a failure must be followed by a retry, not by nothing: {rec:?}"
    );
    assert!(!rec.reason.trim().is_empty(), "the reason is recorded");

    // 2. The next tick inside the backoff window declines, and says until when
    //    and why — the exact line the row asks for.
    let (code2, out2, err2) = ctx.run(&["--apply", "--json", "--timeout", "1"], &env, &url);
    assert_eq!(
        code2, 3,
        "a delayed retry is not an error: stdout={out2} stderr={err2}"
    );
    assert!(err2.contains("update declined until"), "stderr={err2}");
    assert!(
        err2.contains("(last: "),
        "the reason is in the line: {err2}"
    );
    let v: serde_json::Value = serde_json::from_str(&out2).expect("json");
    assert!(
        v["attempt_blocked"]
            .as_str()
            .unwrap_or_default()
            .contains(NEW_VERSION),
        "the refusal names the release: {out2}"
    );
    assert_eq!(
        v["next_retry_at"].as_u64(),
        Some(rec.next_retry_at),
        "the JSON says when the retry happens: {out2}"
    );
    assert_eq!(v["attempts"].as_u64(), Some(1));
    // The rollback also put the OLD binaries back, so the install dir agrees
    // with `current` instead of advertising the failed build as installed.
    assert_eq!(
        update::installed_version(&ctx.state).as_deref(),
        Some(OLD_VERSION)
    );
    assert_eq!(
        feed.hit_count("/gpn-linux.tar.gz"),
        downloads_after_first,
        "a delayed retry must not download the bundle again"
    );

    // 3. Once the clock runs out the same boot retries by itself — no reboot,
    //    no `--force` — and the ladder moves to the next rung (2 h).
    let mut ledger = update::load_attempts(&ctx.state);
    for a in ledger.attempts.iter_mut() {
        if a.version == NEW_VERSION {
            a.next_retry_at = 1; // long past
        }
    }
    update::save_attempts(&ctx.state, &ledger).unwrap();
    let (code3, _out3, err3) = ctx.run(&["--apply", "--timeout", "1"], &env, &url);
    assert_eq!(code3, 1, "the retry really ran (and failed again): {err3}");
    assert!(
        err3.contains("retrying") || err3.contains("NOT CONFIRMED"),
        "the retry is logged as one: {err3}"
    );
    let rec2 = update::attempt_for_version(&ctx.state, NEW_VERSION).unwrap();
    assert_eq!(rec2.attempts, 2, "the second attempt counts: {rec2:?}");
    assert_eq!(
        update::retry_backoff_secs(rec2.attempts),
        7_200,
        "the second failure waits 2 h, not 1 h"
    );
    assert!(rec2.next_retry_at > rec.next_retry_at - 1);

    // 4. A reboot is a shortcut, not the only way out: a new boot id retries
    //    the same release and keeps counting.
    let env2 = [
        ("GPN_UPDATE_RESTART_CMD", "true"),
        ("GPN_BOOT_ID", "boot-next"),
        ("GPN_RPC_URL", rpc.as_str()),
    ];
    let mut ledger = update::load_attempts(&ctx.state);
    for a in ledger.attempts.iter_mut() {
        if a.version == NEW_VERSION {
            a.next_retry_at = 1;
        }
    }
    update::save_attempts(&ctx.state, &ledger).unwrap();
    let (code4, _out4, err4) = ctx.run(&["--apply", "--timeout", "1"], &env2, &url);
    assert_eq!(code4, 1, "attempted again on the new boot: {err4}");
    assert!(!err4.contains("update declined until"), "stderr={err4}");
    let ledger = update::load_attempts(&ctx.state);
    assert!(
        ledger
            .attempts
            .iter()
            .any(|a| a.boot_id == "boot-next" && a.outcome == update::OUTCOME_ROLLED_BACK),
        "the new boot's attempt is recorded: {ledger:?}"
    );
}
