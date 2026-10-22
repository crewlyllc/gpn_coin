//! `/v1/chat/completions` off-chain settlement — behaviour tests.
//!
//! These drive the real AP-side wire path (`hub_ap::try_channel_job_ex`) against
//! a real HTTP provider stub that speaks the provider half of the protocol
//! (`hub_channel::ProviderChannel`). No chain client is supplied: a channel that
//! is already in the registry needs zero chain calls per job, which is exactly
//! the property under test (the on-chain path is *not* taken, so `NextJobId`
//! cannot advance — the live measurement asserts that against the real node).

use gpn_agent::chain::signer::Sr25519Signer;
use gpn_agent::hub_ap::{ChannelFail, HubCtx};
use gpn_agent::hub_channel::{ChannelState, HubChannel, HubRegistry, ProviderChannel, PubKey};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const DEPOSIT: u128 = 1_000_000;

/// What the stub provider should do with the assignment it receives.
#[derive(Clone, Copy, PartialEq)]
enum Behaviour {
    Serve,
    /// Serve, but hold the request for 250 ms first — a render's worth of time,
    /// so two concurrent AP threads really do overlap.
    ServeSlow,
    /// Answer the *first* `POST /channel/job` with the provider's real
    /// stale-pre-state refusal, then serve. Reproduces the live 409.
    StaleOnce,
    Refund,
    GarbageResponse,
    Reject409,
}

struct Stub {
    port: u16,
    /// How many `POST /channel/job` requests were delivered.
    hits: Arc<AtomicU64>,
    _handle: std::thread::JoinHandle<()>,
}

/// Start a provider stub. `behaviour` is fixed for the whole server lifetime,
/// which is enough for one request per test.
fn start_stub(
    behaviour: Behaviour,
    ap_pub: PubKey,
    provider_signer: Arc<Sr25519Signer>,
    deposit: u128,
) -> Stub {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("bind stub");
    let port = server.server_addr().to_ip().unwrap().port();
    let hits = Arc::new(AtomicU64::new(0));
    let hits_c = hits.clone();
    let handle = std::thread::spawn(move || {
        let mut chans: HashMap<u64, ProviderChannel> = HashMap::new();
        let mut seen: u64 = 0;
        for mut req in server.incoming_requests() {
            let path = req.url().split('?').next().unwrap_or("").to_string();
            if req.method() == &tiny_http::Method::Get && path == "/channel/status" {
                let body = serde_json::json!({"ok": true, "channel": true});
                let _ = req.respond(tiny_http::Response::from_string(body.to_string()));
                continue;
            }
            if req.method() != &tiny_http::Method::Post || path != "/channel/job" {
                let _ = req.respond(
                    tiny_http::Response::from_string("not found").with_status_code(404),
                );
                continue;
            }
            hits_c.fetch_add(1, Ordering::SeqCst);
            seen += 1;
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
            if behaviour == Behaviour::StaleOnce && seen == 1 {
                // Exactly what the diffusion provider answers when another job
                // advanced the channel first.
                let _ = req.respond(
                    tiny_http::Response::from_string(
                        serde_json::json!({
                            "ok": false,
                            "error": "assignment rejected: state_before is not the current channel state"
                        })
                        .to_string(),
                    )
                    .with_status_code(409),
                );
                continue;
            }
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
                .or_insert_with(|| ProviderChannel::new(a.channel_id, deposit, ap_pub));
            if let Err(e) = ch.accept(&a) {
                let _ = req.respond(
                    tiny_http::Response::from_string(
                        serde_json::json!({"ok": false, "error": e.to_string()}).to_string(),
                    )
                    .with_status_code(409),
                );
                continue;
            }
            let msg = match behaviour {
                Behaviour::Serve => ch.sign_result(&provider_signer, a.job_id, "hello", 3, 4),
                Behaviour::ServeSlow => {
                    std::thread::sleep(Duration::from_millis(250));
                    ch.sign_result(&provider_signer, a.job_id, "hello", 3, 4)
                }
                Behaviour::Refund => ch.sign_refund(&provider_signer, a.job_id, "ollama exploded"),
                _ => ch.sign_result(&provider_signer, a.job_id, "hello", 3, 4),
            };
            let body = match (behaviour, msg) {
                (Behaviour::GarbageResponse, _) => "{}".to_string(),
                (_, Ok(m)) => serde_json::to_string(&m).unwrap(),
                (_, Err(e)) => serde_json::json!({"ok": false, "error": e.to_string()}).to_string(),
            };
            let _ = req.respond(tiny_http::Response::from_string(body));
        }
    });
    Stub {
        port,
        hits,
        _handle: handle,
    }
}

struct Fixture {
    hub: Arc<HubCtx>,
    provider_key: String,
    stub: Stub,
    state_path: std::path::PathBuf,
}

fn fixture(behaviour: Behaviour, enabled: bool) -> Fixture {
    let ap = Sr25519Signer::from_uri("//Alice").unwrap();
    let prov = Arc::new(Sr25519Signer::from_uri("//Bob").unwrap());
    let provider_key = PubKey(prov.public_key()).to_hex();
    let stub = start_stub(
        behaviour,
        PubKey(ap.public_key()),
        prov.clone(),
        DEPOSIT,
    );

    let dir = std::env::temp_dir().join(format!(
        "gpn-chan-{}-{}",
        std::process::id(),
        stub.port
    ));
    let _ = std::fs::create_dir_all(&dir);
    let state_path = dir.join("hub_channels.json");

    // Pre-seed a live channel, so the per-job path needs no chain RPC at all.
    let mut reg = HubRegistry::default();
    reg.channels
        .insert(provider_key.clone(), HubChannel::new(42, DEPOSIT));
    assert!(reg.save(&state_path));

    let hub = Arc::new(HubCtx {
        enabled,
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

fn status_url(port: u16) -> String {
    // The channel endpoint is heartbeat_port + 1, so subtract one here.
    format!("http://127.0.0.1:{}/status", port - 1)
}

fn run(f: &Fixture, price: u128) -> Result<gpn_agent::hub_ap::ChannelJobOut, ChannelFail> {
    run_billed(f, price, None)
}

fn run_billed(
    f: &Fixture,
    price: u128,
    billing: Option<gpn_agent::hub_channel::BillingRates>,
) -> Result<gpn_agent::hub_ap::ChannelJobOut, ChannelFail> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let started = Instant::now();
    let out = gpn_agent::hub_ap::try_channel_job_billed_ex(
        &f.hub,
        &rt,
        None, // no chain client: a per-job channel settle needs none
        &f.provider_key,
        &status_url(f.stub.port),
        "llama3.2:1b",
        "say hi",
        32,
        price,
        billing,
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "a channel job must not wait for block inclusion"
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
fn happy_path_settles_off_chain_with_no_create() {
    let f = fixture(Behaviour::Serve, true);
    let out = run(&f, 10).expect("channel job succeeds");
    assert_eq!(out.text, "hello");
    assert_eq!(out.usage, Some((3, 4)));
    let ch = channel(&f);
    assert_eq!(ch.state.balance_provider, 10, "provider was paid off-chain");
    assert_eq!(ch.state.balance_requestor, DEPOSIT - 10);
    assert_eq!(ch.state.nonce, 1, "one signed state update, no tx");
    assert_eq!(ch.pending_len(), 0, "no Pending job left behind");
    assert!(ch.state.is_conserved(DEPOSIT));
    assert_eq!(f.stub.hits.load(Ordering::SeqCst), 1);
    let _ = std::fs::remove_dir_all(f.state_path.parent().unwrap());
}

#[test]
fn hub_disabled_falls_back_before_touching_the_provider() {
    let f = fixture(Behaviour::Serve, false);
    match run(&f, 10) {
        Err(e) => {
            assert!(e.fallback_ok(), "disabled hub must be safe to fall back");
            assert!(matches!(e, ChannelFail::NotAttempted(_)));
        }
        Ok(_) => panic!("channel must not be used when the hub is disabled"),
    }
    assert_eq!(
        f.stub.hits.load(Ordering::SeqCst),
        0,
        "no request may be sent when the hub is disabled"
    );
    let ch = channel(&f);
    assert_eq!(ch.state.nonce, 0);
    assert_eq!(ch.pending_len(), 0);
    let _ = std::fs::remove_dir_all(f.state_path.parent().unwrap());
}

#[test]
fn provider_failure_signs_a_refund_so_fallback_cannot_double_charge() {
    let f = fixture(Behaviour::Refund, true);
    match run(&f, 10) {
        Err(e) => assert!(
            e.fallback_ok(),
            "a refunded job moved no value; on-chain fallback is safe"
        ),
        Ok(_) => panic!("a refunded job is not a success"),
    }
    let ch = channel(&f);
    assert_eq!(ch.state.balance_provider, 0, "refund restored the provider balance");
    assert_eq!(ch.state.balance_requestor, DEPOSIT);
    assert_eq!(ch.state.nonce, 2, "refund advances the nonce by two");
    assert_eq!(ch.pending_len(), 0, "refunded job is not left pending");
    assert!(ch.state.is_conserved(DEPOSIT));
    let _ = std::fs::remove_dir_all(f.state_path.parent().unwrap());
}

#[test]
fn rejected_assignment_aborts_the_pending_entry() {
    let f = fixture(Behaviour::Reject409, true);
    match run(&f, 10) {
        Err(e) => assert!(e.fallback_ok(), "a 4xx at accept-time means nothing was paid"),
        Ok(_) => panic!("provider rejected the assignment"),
    }
    let ch = channel(&f);
    assert_eq!(ch.pending_len(), 0, "rejected assignment must not stay pending");
    assert_eq!(ch.state.nonce, 0, "no state was advanced");
    assert_eq!(
        f.stub.hits.load(Ordering::SeqCst),
        1,
        "a 409 that does not mention the channel state is a per-job rejection, not a stale-state retry"
    );
    let _ = std::fs::remove_dir_all(f.state_path.parent().unwrap());
}

#[test]
fn a_stale_channel_state_is_retried_not_treated_as_a_failure() {
    // The provider answers the first submission with its real refusal
    // (`state_before is not the current channel state`) and serves the retry.
    // The AP must re-price and resubmit — nothing was charged on the 409.
    let f = fixture(Behaviour::StaleOnce, true);
    let out = run(&f, 10).expect("the retry must succeed");
    assert_eq!(out.text, "hello");
    assert_eq!(
        f.stub.hits.load(Ordering::SeqCst),
        2,
        "exactly one retry after the stale-state 409"
    );
    let ch = channel(&f);
    assert_eq!(ch.state.balance_provider, 10);
    assert_eq!(ch.state.nonce, 1);
    assert_eq!(ch.pending_len(), 0);
    let _ = std::fs::remove_dir_all(f.state_path.parent().unwrap());
}

#[test]
fn concurrent_jobs_on_one_channel_take_turns_instead_of_racing() {
    // The live bug: two overlapping requests both price off the same channel
    // state (`begin_job` does not advance it — only an applied result does), so
    // the provider rejects the second with `StaleStateBefore` (HTTP 409) and the
    // caller fell back. The per-channel submission gate must make them queue.
    let f = fixture(Behaviour::ServeSlow, true);
    let hub = f.hub.clone();
    let key = f.provider_key.clone();
    let port = f.stub.port;
    let mut handles = Vec::new();
    for _ in 0..2 {
        let hub = hub.clone();
        let key = key.clone();
        handles.push(std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            gpn_agent::hub_ap::try_channel_job_ex(
                &hub,
                &rt,
                None,
                &key,
                &status_url(port),
                "llama3.2:1b",
                "say hi",
                32,
                10,
            )
        }));
    }
    let mut outcomes = Vec::new();
    for h in handles {
        outcomes.push(h.join().expect("worker thread"));
    }
    for o in &outcomes {
        match o {
            Ok(_) => {}
            Err(e) => panic!("concurrent job failed: {e:?}"),
        }
    }
    let ch = channel(&f);
    assert_eq!(ch.state.balance_provider, 20, "both jobs were paid");
    assert_eq!(ch.state.nonce, 2, "one signed state update per job, no collision");
    assert_eq!(ch.pending_len(), 0);
    assert_eq!(
        f.stub.hits.load(Ordering::SeqCst),
        2,
        "neither job was rejected, so neither needed a retry"
    );
    let _ = std::fs::remove_dir_all(f.state_path.parent().unwrap());
}

#[test]
fn unreadable_result_is_ambiguous_and_never_falls_back() {
    let f = fixture(Behaviour::GarbageResponse, true);
    match run(&f, 10) {
        Err(ChannelFail::Ambiguous(_)) => {}
        other => panic!("expected an ambiguous failure, got {other:?}"),
    }
    let ch = channel(&f);
    // The provider may have run the work; keep the entry so it expires on its
    // own deadline instead of being silently reused.
    assert_eq!(ch.pending_len(), 1);
    assert_eq!(ch.state.balance_provider, 0, "no local payment was applied");
    let _ = std::fs::remove_dir_all(f.state_path.parent().unwrap());
}

#[test]
fn too_small_a_channel_balance_is_a_safe_fallback() {
    let f = fixture(Behaviour::Serve, true);
    match run(&f, DEPOSIT + 1) {
        Err(e) => assert!(e.fallback_ok()),
        Ok(_) => panic!("the channel cannot fund this job"),
    }
    assert_eq!(
        f.stub.hits.load(Ordering::SeqCst),
        0,
        "an unaffordable job never reaches the provider"
    );
    assert_eq!(channel(&f).pending_len(), 0);
    let _ = std::fs::remove_dir_all(f.state_path.parent().unwrap());
}

#[test]
fn a_billed_job_charges_actual_usage_through_the_real_wire_path() {
    // The stub provider answers with (3 prompt, 4 completion) tokens. At
    // 10/100 per token that is 430, well under the 10_000 ceiling: the AP must
    // apply 430 to the channel, not the committed maximum.
    let f = fixture(Behaviour::Serve, true);
    let rates = gpn_agent::hub_channel::BillingRates {
        in_rate: 10,
        out_rate: 100,
    };
    let out = run_billed(&f, 10_000, Some(rates)).expect("billed channel job succeeds");
    assert_eq!(out.charge, 430);
    assert_eq!(out.usage, Some((3, 4)));
    let ch = channel(&f);
    assert_eq!(ch.state.balance_provider, 430, "charged actual, not the ceiling");
    assert_eq!(ch.state.balance_requestor, DEPOSIT - 430);
    assert_eq!(ch.state.nonce, 1, "a cheaper settlement is the same update");
    assert_eq!(ch.pending_len(), 0);
    assert!(ch.state.is_conserved(DEPOSIT));
    let _ = std::fs::remove_dir_all(f.state_path.parent().unwrap());
}

#[test]
fn a_ceiling_it_cannot_reach_is_still_billed_as_the_ceiling() {
    // Ceiling below the actual cost (3*10 + 4*100 = 430): the cap wins.
    let f = fixture(Behaviour::Serve, true);
    let rates = gpn_agent::hub_channel::BillingRates {
        in_rate: 10,
        out_rate: 100,
    };
    let out = run_billed(&f, 100, Some(rates)).expect("billed channel job succeeds");
    assert_eq!(out.charge, 100);
    assert_eq!(channel(&f).state.balance_provider, 100);
    let _ = std::fs::remove_dir_all(f.state_path.parent().unwrap());
}

#[test]
fn a_channel_state_starts_from_the_seeded_onchain_view() {
    // Sanity: the fixture's channel is the classic deposit-split state, so the
    // numbers above mean what they say.
    let f = fixture(Behaviour::Serve, true);
    let ch = channel(&f);
    let want = ChannelState::new(DEPOSIT);
    assert_eq!(ch.state, want);
    assert_eq!(ch.channel_id, 42);
    let _ = std::fs::remove_dir_all(f.state_path.parent().unwrap());
}
