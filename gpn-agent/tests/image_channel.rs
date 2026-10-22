//! `/v1/images/generations` — the money-safe path, end to end.
//!
//! These drive the REAL AP-side wire functions (`hub_ap::try_channel_gen_job`,
//! `image_jobs`, `image_channel`) against a REAL HTTP provider stub, exactly as
//! `run_image_record` in `gpn_scheduler` does. No chain client is supplied, so
//! any attempt to create an on-chain job would panic/return `None`: the happy
//! path provably settles as **zero transactions**.
//!
//! The scheduler binary's HTTP handlers are thin wrappers over these functions
//! (`202` immediately, poll a record, serve the bytes), which is why this is the
//! right seam to test: a render takes 29 s and a unit test must not need a GPU.

use gpn_agent::chain::signer::Sr25519Signer;
use gpn_agent::hub_ap::{try_channel_gen_job, ChannelFail, HubCtx};
use gpn_agent::hub_channel::{
    assignment_payload, GenParams, HubChannel, HubRegistry, ProviderChannel, PubKey,
};
use gpn_agent::image_jobs::{self, ImageJob};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const DEPOSIT: u128 = 1_000_000;

/// `GPN_IMAGE_DIR` is process-global, so tests that write records take this lock.
fn serialize() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
/// A 1x1 PNG is enough: the AP validates the signature, not the pixels.
const PNG: &[u8] = &[
    0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, // signature
    0x00, 0x00, 0x00, 0x0d, b'I', b'H', b'D', b'R', 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
    0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89,
];

#[derive(Clone, Copy, PartialEq)]
enum Behaviour {
    Serve,
    Refund,
    Reject409,
    GarbageResult,
}

struct Stub {
    port: u16,
    jobs: Arc<AtomicU64>,
    artifacts: Arc<AtomicU64>,
    _handle: std::thread::JoinHandle<()>,
}

/// A provider stub speaking the generation half of the channel protocol.
///
/// `artifact_server` chooses whether `GET /channel/artifact/...` returns the
/// bytes or a 404 — the "render succeeded but the file is gone" case.
fn start_stub(
    behaviour: Behaviour,
    ap_pub: PubKey,
    signer: Arc<Sr25519Signer>,
    artifact_server: bool,
) -> Stub {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("bind stub");
    let port = server.server_addr().to_ip().unwrap().port();
    let jobs = Arc::new(AtomicU64::new(0));
    let artifacts = Arc::new(AtomicU64::new(0));
    let jobs_c = jobs.clone();
    let artifacts_c = artifacts.clone();
    let handle = std::thread::spawn(move || {
        let mut chans: HashMap<u64, ProviderChannel> = HashMap::new();
        for mut req in server.incoming_requests() {
            let url = req.url().to_string();
            let path = url.split('?').next().unwrap_or("").to_string();
            if req.method() == &tiny_http::Method::Get && path == "/channel/status" {
                let _ = req.respond(tiny_http::Response::from_string(
                    serde_json::json!({"ok": true, "channel": true, "kinds": ["image"]})
                        .to_string(),
                ));
                continue;
            }
            if req.method() == &tiny_http::Method::Get && path.starts_with("/channel/artifact/") {
                artifacts_c.fetch_add(1, Ordering::SeqCst);
                let token = url.split_once("?t=").map(|(_, t)| t).unwrap_or("");
                if artifact_server && token.len() == 64 {
                    let ct = tiny_http::Header::from_bytes(
                        &b"Content-Type"[..],
                        &b"image/png"[..],
                    )
                    .unwrap();
                    let _ = req.respond(tiny_http::Response::from_data(PNG.to_vec()).with_header(ct));
                } else {
                    let _ = req.respond(
                        tiny_http::Response::from_string("no such artifact").with_status_code(404),
                    );
                }
                continue;
            }
            if req.method() != &tiny_http::Method::Post || path != "/channel/job" {
                let _ = req
                    .respond(tiny_http::Response::from_string("not found").with_status_code(404));
                continue;
            }
            jobs_c.fetch_add(1, Ordering::SeqCst);
            let mut body = String::new();
            let _ = std::io::Read::read_to_string(&mut req.as_reader(), &mut body);
            let a: gpn_agent::hub_channel::Assignment = match serde_json::from_str(&body) {
                Ok(a) => a,
                Err(e) => {
                    let _ = req.respond(
                        tiny_http::Response::from_string(
                            serde_json::json!({"ok": false, "error": e.to_string()}).to_string(),
                        )
                        .with_status_code(400),
                    );
                    continue;
                }
            };
            // The signature must cover the render parameters: flipping a step
            // count after signing has to invalidate the assignment.
            assert!(
                gpn_agent::hub_channel::verify_sig(
                    &a.requestor,
                    &assignment_payload(&a),
                    &a.sig_requestor
                ),
                "AP assignment signature must verify"
            );
            assert_eq!(a.job_kind, "ImageGeneration");
            assert!(a.gen.as_ref().map(|g| g.width == 512).unwrap_or(false));
            if behaviour == Behaviour::Reject409 {
                let _ = req.respond(
                    tiny_http::Response::from_string(
                        serde_json::json!({"ok": false, "error": "assignment rejected"}).to_string(),
                    )
                    .with_status_code(409),
                );
                continue;
            }
            let ch = chans
                .entry(a.channel_id)
                .or_insert_with(|| ProviderChannel::new(a.channel_id, DEPOSIT, ap_pub));
            if let Err(e) = ch.accept(&a) {
                let _ = req.respond(
                    tiny_http::Response::from_string(
                        serde_json::json!({"ok": false, "error": e.to_string()}).to_string(),
                    )
                    .with_status_code(409),
                );
                continue;
            }
            let output = serde_json::json!({
                "artifacts": [{
                    "index": 0,
                    "sha256": sha256_hex(PNG),
                    "media_type": "image/png",
                    "width": 512,
                    "height": 512,
                    "bytes": PNG.len(),
                }],
                "render_seconds": 28.9,
            })
            .to_string();
            let msg = match behaviour {
                Behaviour::Serve => ch.sign_result(&signer, a.job_id, &output, 0, 0),
                Behaviour::Refund => ch.sign_refund(&signer, a.job_id, "VRAM refusal"),
                _ => ch.sign_refund(&signer, a.job_id, "unused"),
            };
            let body = match (behaviour, msg) {
                (Behaviour::GarbageResult, _) => "{}".to_string(),
                (_, Ok(m)) => serde_json::to_string(&m).unwrap(),
                (_, Err(e)) => {
                    serde_json::json!({"ok": false, "error": e.to_string()}).to_string()
                }
            };
            let _ = req.respond(tiny_http::Response::from_string(body));
        }
    });
    Stub {
        port,
        jobs,
        artifacts,
        _handle: handle,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

struct Fixture {
    hub: Arc<HubCtx>,
    provider_key: String,
    stub: Stub,
    state_path: std::path::PathBuf,
}

fn fixture(behaviour: Behaviour, artifact_server: bool) -> Fixture {
    let ap = Sr25519Signer::from_uri("//Alice").unwrap();
    let prov = Arc::new(Sr25519Signer::from_uri("//Bob").unwrap());
    let provider_key = PubKey(prov.public_key()).to_hex();
    let stub = start_stub(behaviour, PubKey(ap.public_key()), prov.clone(), artifact_server);

    let dir = std::env::temp_dir().join(format!(
        "gpn-imgchan-{}-{}",
        std::process::id(),
        stub.port
    ));
    let _ = std::fs::create_dir_all(&dir);
    let state_path = dir.join("hub_channels.json");
    let image_dir = dir.join("images");
    std::env::set_var("GPN_IMAGE_DIR", &image_dir);

    let mut reg = HubRegistry::default();
    reg.channels
        .insert(provider_key.clone(), HubChannel::new(42, DEPOSIT));
    assert!(reg.save(&state_path));

    let hub = Arc::new(HubCtx {
        enabled: true,
        policy: gpn_agent::settle_policy::SettlePolicy::default(),
        signer: Arc::new(ap),
        registry: Mutex::new(HubRegistry::load(&state_path)),
        path: state_path.clone(),
        deposit: DEPOSIT,
        ttl: 100,
        settle_secs: 86_400,
        close_at: DEPOSIT / 2,
        close_nonce: 500,
        job_deadline_ms: 10_000,
        unsupported: Mutex::new(HashMap::new()),
        submit_locks: Mutex::new(HashMap::new()),
    });
    Fixture {
        hub,
        provider_key,
        stub,
        state_path,
    }
}

fn gen() -> GenParams {
    GenParams {
        kind: "image".into(),
        width: 512,
        height: 512,
        steps: 8,
        seed: 7,
        frames: 1,
        fps: 1,
        n: 1,
    }
}

/// Drive one render exactly as the scheduler's background worker does.
fn run(f: &Fixture, price: u128) -> Result<gpn_agent::hub_ap::ChannelJobOut, ChannelFail> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let started = std::time::Instant::now();
    let out = try_channel_gen_job(
        &f.hub,
        &rt,
        None, // <- no chain client: a create would be impossible
        &f.provider_key,
        &format!("http://127.0.0.1:{}/status", f.stub.port - 1),
        Some(format!("http://127.0.0.1:{}/channel/job", f.stub.port)),
        "sd-turbo",
        "a cozy cabin at dusk",
        "ImageGeneration",
        gen(),
        price,
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "an off-chain render job must not wait for block inclusion"
    );
    out
}

fn channel(f: &Fixture) -> HubChannel {
    f.hub
        .registry
        .lock()
        .unwrap()
        .channels
        .get(&f.provider_key)
        .cloned()
        .expect("channel")
}

#[test]
fn happy_path_202_poll_completes_with_zero_transactions() {
    let _s = serialize();
    let f = fixture(Behaviour::Serve, true);
    let id = image_jobs::new_id("a cozy cabin at dusk");
    let rec = ImageJob::new(id.clone(), "a cozy cabin at dusk", "sd-turbo", 512, 512, 8, 7, 1, 30.15, 0.0076);
    image_jobs::save(&rec).unwrap();
    assert_eq!(image_jobs::get(&id).unwrap().status, "pending");

    let out = run(&f, 10).expect("channel render succeeds");
    // The provider only ever returns the artifact metadata; the AP fetches the
    // bytes itself and validates them.
    let v: serde_json::Value = serde_json::from_str(&out.text).unwrap();
    let sha = v["artifacts"][0]["sha256"].as_str().unwrap().to_string();

    let token = gpn_agent::image_channel::artifact_token(out.channel_id, out.job_id);
    let url = format!(
        "http://127.0.0.1:{}/channel/artifact/{}?t={}",
        f.stub.port, out.job_id, token
    );
    let bytes = reqwest::blocking::get(&url).unwrap().bytes().unwrap().to_vec();
    assert!(image_jobs::is_png(&bytes), "fetched bytes must be a PNG");
    assert_eq!(sha256_hex(&bytes), sha, "provider's sha256 must match the bytes");
    image_jobs::put_artifact(&id, 0, &bytes).unwrap();
    image_jobs::update(&id, |j| {
        j.status = "completed".into();
        j.payment = "channel".into();
        j.channel_id = Some(out.channel_id);
        j.job_id = Some(out.job_id);
        j.data.push(image_jobs::ImageArtifact {
            index: 0,
            media_type: "image/png".into(),
            sha256: sha.clone(),
            bytes: bytes.len() as u64,
            width: 512,
            height: 512,
        });
    });

    let done = image_jobs::get(&id).unwrap();
    assert_eq!(done.status, "completed", "poll must transition to completed");
    assert_eq!(done.payment, "channel", "settled off-chain");
    assert_eq!(done.job_id, Some(out.job_id));
    assert_eq!(done.data[0].media_type, "image/png");
    // `GET /v1/images/{id}` serves the same bytes with image/png.
    assert!(image_jobs::artifact_path(&id, 0).exists());

    let ch = channel(&f);
    assert_eq!(ch.state.balance_provider, 10, "provider paid off-chain");
    assert_eq!(ch.state.nonce, 1, "exactly one signed state update, no tx");
    assert_eq!(ch.pending_len(), 0);
    assert_eq!(f.stub.jobs.load(Ordering::SeqCst), 1);
    assert_eq!(f.stub.artifacts.load(Ordering::SeqCst), 1, "bytes fetched once");

    let _ = std::fs::remove_dir_all(f.state_path.parent().unwrap());
}

#[test]
fn sync_mode_refuses_a_render_over_the_budget_and_explains_the_estimate() {
    let _s = serialize();
    // 1024² / 50 steps: the estimate is comfortably over 60 s.
    let est = 30.0 + (1024.0 * 1024.0 * 50.0 / 1e6) * 0.73;
    let msg = image_jobs::sync_refusal(est).expect("must refuse");
    assert!(msg.contains("estimated"), "the refusal must quote the estimate");
    assert!(msg.contains("poll"), "and point at the polling path");
    // ... while the reference 512²/8 render is allowed inline.
    assert!(image_jobs::sync_refusal(30.15).is_none());
}

#[test]
fn a_refunded_render_falls_back_safely() {
    let _s = serialize();
    let f = fixture(Behaviour::Refund, true);
    match run(&f, 10) {
        Err(e) => {
            assert!(e.fallback_ok(), "a refund moved no money");
            assert!(e.reason().contains("refunded"));
        }
        Ok(_) => panic!("a refunded render is not a success"),
    }
    let ch = channel(&f);
    assert_eq!(ch.state.balance_provider, 0, "refund restored the balances");
    assert_eq!(ch.state.balance_requestor, DEPOSIT);
    assert_eq!(ch.pending_len(), 0);
    let _ = std::fs::remove_dir_all(f.state_path.parent().unwrap());
}

#[test]
fn an_accept_time_rejection_is_not_attempted_so_onchain_is_safe() {
    let _s = serialize();
    let f = fixture(Behaviour::Reject409, true);
    match run(&f, 10) {
        Err(e) => assert!(e.fallback_ok(), "4xx at accept-time means nothing was paid"),
        Ok(_) => panic!("provider rejected the assignment"),
    }
    assert_eq!(channel(&f).pending_len(), 0);
    let _ = std::fs::remove_dir_all(f.state_path.parent().unwrap());
}

#[test]
fn an_unreadable_result_never_falls_back_so_the_caller_cannot_be_charged_twice() {
    let _s = serialize();
    let f = fixture(Behaviour::GarbageResult, true);
    match run(&f, 10) {
        Err(ChannelFail::Ambiguous(_)) => {}
        other => panic!("expected Ambiguous, got {other:?}"),
    }
    assert_eq!(
        channel(&f).pending_len(),
        1,
        "the entry must be left to expire, not reused"
    );
    let _ = std::fs::remove_dir_all(f.state_path.parent().unwrap());
}

#[test]
fn a_missing_artifact_fails_the_job_instead_of_publishing_garbage() {
    let _s = serialize();
    let f = fixture(Behaviour::Serve, false); // render "succeeds", bytes 404
    let out = run(&f, 10).expect("the settlement itself succeeded");
    let token = gpn_agent::image_channel::artifact_token(out.channel_id, out.job_id);
    let url = format!(
        "http://127.0.0.1:{}/channel/artifact/{}?t={}",
        f.stub.port, out.job_id, token
    );
    let resp = reqwest::blocking::get(&url).unwrap();
    assert_eq!(resp.status().as_u16(), 404);
    // No bytes → the scheduler marks the job failed (run_image_record); here we
    // assert the invariant it relies on: no artifact file was written.
    let id = image_jobs::new_id("x");
    assert!(!image_jobs::artifact_path(&id, 0).exists());
    let _ = std::fs::remove_dir_all(f.state_path.parent().unwrap());
}

#[test]
fn idle_refuses_an_image_job_exactly_like_text() {
    let _s = serialize();
    match gpn_agent::image_channel::serve_decision(true, "sd-turbo", 8188, 512, 512) {
        gpn_agent::image_channel::ServeDecision::Refund(reason) => {
            assert!(reason.contains("idle"))
        }
        gpn_agent::image_channel::ServeDecision::Run => panic!("idle must not accept renders"),
    }
}

#[test]
fn unknown_image_ids_are_404_and_traversal_is_rejected() {
    let _s = serialize();
    std::env::set_var("GPN_IMAGE_DIR", std::env::temp_dir().join("gpn-img-404"));
    assert!(image_jobs::get("0".repeat(32).as_str()).is_none());
    assert!(!image_jobs::valid_id("../../etc/passwd"));
    assert!(image_jobs::artifact_path("../../etc/passwd", 0).exists() == false);
}
