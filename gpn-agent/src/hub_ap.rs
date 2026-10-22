//! AP-side hub-channel plumbing (item #2, `design-offchain-escrow-channels.md`
//! v2). Kept in its own module so the (already large) scheduler handler only
//! gains one small branch: "try the channel path, else fall through to the
//! on-chain escrow path".
//!
//! On-chain interaction is limited to:
//!   * `open_channel` — **once** per (AP, provider) pair, when no live channel
//!     exists (looked up via `ChannelIndex`, so a restart adopts rather than
//!     re-opens);
//!   * `close_channel` — cooperatively, when the provider balance crosses a
//!     threshold or the nonce gap is large.
//!
//! The job itself is: signed `Assignment` → HTTP POST to the provider's
//! `/channel/job` → signed `JobResultMsg` → local state update. Zero extrinsics.

use crate::chain::signer::Sr25519Signer;
use crate::chain::{ChainClient, OnChainChannel};
use crate::hub_channel::{
    now_ms, Assignment, ChannelState, HubChannel, HubRegistry, JobResultMsg, PubKey,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Everything the handler needs to route a job off-chain.
pub struct HubCtx {
    pub enabled: bool,
    pub signer: Arc<Sr25519Signer>,
    pub registry: Mutex<HubRegistry>,
    pub path: std::path::PathBuf,
    /// Deposit locked when a new hub channel is opened (base units).
    pub deposit: u128,
    /// Channel TTL in blocks.
    pub ttl: u32,
    /// Cooperative close when the provider's off-chain balance reaches this.
    pub close_at: u128,
    /// ...or when this many off-chain updates have accumulated.
    pub close_nonce: u64,
    /// Per-job wall-clock deadline for the provider (inference + return).
    pub job_deadline_ms: u64,
    /// Providers whose `/channel/job` endpoint refused/failed, with a retry-after.
    pub unsupported: Mutex<HashMap<String, Instant>>,
}

impl HubCtx {
    pub fn from_env(signer_uri: &str) -> Arc<Self> {
        let mode = std::env::var("GPN_ESCROW_MODE")
            .unwrap_or_else(|_| "channel".to_string())
            .to_ascii_lowercase();
        let deposit: u128 = std::env::var("GPN_HUB_CHANNEL_DEPOSIT")
            .ok()
            .and_then(|s| s.parse().ok())
            // 100 GPN — an AP-sized working balance, not a whale channel.
            .unwrap_or(100_000_000_000_000);
        let ttl: u32 = std::env::var("GPN_HUB_CHANNEL_TTL")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(25_920); // ~4.5 days at 15 s blocks
        let close_at: u128 = std::env::var("GPN_HUB_CLOSE_AT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(deposit / 2);
        let close_nonce: u64 = std::env::var("GPN_HUB_CLOSE_NONCE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(500);
        let job_deadline_ms: u64 = std::env::var("GPN_HUB_JOB_DEADLINE_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(180_000);
        let path = match std::env::var("GPN_HUB_STATE") {
            Ok(p) => std::path::PathBuf::from(p),
            Err(_) => {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
                std::path::PathBuf::from(format!("{home}/.gpn/hub_channels.json"))
            }
        };
        let signer = Sr25519Signer::from_uri(signer_uri).expect("hub signer");
        let enabled = mode != "onchain" && mode != "off" && deposit > 0;
        if enabled {
            println!(
                "[hub] off-chain escrow ENABLED (channel/{mode}, deposit={deposit}, ttl={ttl} blocks, state={})",
                path.display()
            );
        } else {
            println!("[hub] off-chain escrow disabled (GPN_ESCROW_MODE={mode}, deposit={deposit}) — per-job on-chain escrow");
        }
        Arc::new(Self {
            enabled,
            signer: Arc::new(signer),
            registry: Mutex::new(HubRegistry::load(&path)),
            path,
            deposit,
            ttl,
            close_at,
            close_nonce,
            job_deadline_ms,
            unsupported: Mutex::new(HashMap::new()),
        })
    }

    fn mark_unsupported(&self, provider: &str) {
        self.unsupported.lock().unwrap().insert(
            provider.to_string(),
            Instant::now() + Duration::from_secs(60),
        );
    }

    fn is_unsupported(&self, provider: &str) -> bool {
        let mut m = self.unsupported.lock().unwrap();
        match m.get(provider) {
            Some(until) if *until > Instant::now() => true,
            Some(_) => {
                m.remove(provider);
                false
            }
            None => false,
        }
    }
}

/// The provider serves the channel endpoint one port above its `/status`
/// heartbeat (8093 → 8094), same convention as the passthrough `/infer` call.
pub fn channel_endpoint(status_url: &str) -> Option<String> {
    let u = status_url.trim_end_matches('/');
    let u = u.strip_suffix("/status").unwrap_or(u);
    let (scheme, rest) = u.split_once("://")?;
    let hostport = rest.split('/').next()?;
    let (host, port) = hostport.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    Some(format!(
        "{scheme}://{host}:{}/channel/job",
        port.checked_add(1)?
    ))
}

/// Ask the provider whether it serves the channel protocol, **before** the AP
/// opens (and funds) a channel. A provider on an older build 404s here, so no
/// deposit is ever locked against a peer that cannot use it.
pub fn probe_channel_support(status_url: &str) -> bool {
    let Some(job_url) = channel_endpoint(status_url) else {
        return false;
    };
    let Some(base) = job_url.strip_suffix("/channel/job") else {
        return false;
    };
    let url = format!("{base}/channel/status");
    let Ok(c) = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(2_500))
        .build()
    else {
        return false;
    };
    match c.get(&url).send() {
        Ok(r) if r.status().is_success() => r
            .json::<serde_json::Value>()
            .map(|v| v.get("channel").and_then(|x| x.as_bool()).unwrap_or(false))
            .unwrap_or(false),
        _ => false,
    }
}

#[derive(Debug)]
pub struct ChannelJobOut {
    pub channel_id: u64,
    pub job_id: u64,
    pub text: String,
    pub usage: Option<(u32, u32)>,
    pub balance_provider: u128,
}

/// Why a channel job did not complete.
///
/// The distinction is a **money-safety** one, not a cosmetic one:
///
/// * [`ChannelFail::NotAttempted`] — no value moved. The assignment never
///   reached a provider (hub disabled, no channel capacity, provider does not
///   speak the protocol, connect error) or the provider explicitly rejected it
///   at accept-time (HTTP 4xx), or it refunded the job after failing mid-run.
///   The caller **must** fall back to the on-chain path.
/// * [`ChannelFail::Ambiguous`] — the assignment was delivered and may have
///   been accepted. The provider holds the AP's pre-signed `sig_settle`, so it
///   can force-close and collect even if we never saw the result. Falling back
///   to on-chain here would charge the caller **twice**. The caller must
///   surface an error and leave the pending entry to expire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelFail {
    /// Nothing was paid; safe to use the on-chain escrow path instead.
    NotAttempted(String),
    /// Possibly accepted by the provider; do NOT pay a second time.
    Ambiguous(String),
}

impl ChannelFail {
    /// True when the caller may safely fall back to the on-chain path.
    pub fn fallback_ok(&self) -> bool {
        matches!(self, ChannelFail::NotAttempted(_))
    }
    pub fn reason(&self) -> &str {
        match self {
            ChannelFail::NotAttempted(s) | ChannelFail::Ambiguous(s) => s,
        }
    }
}

/// Route one job through the hub channel, reporting *why* it failed.
///
/// `client` may be `None` when a live channel already exists in the registry
/// (no chain call is needed for a per-job settle); tests use that to exercise
/// the wire path with no node.
#[allow(clippy::too_many_arguments)]
pub fn try_channel_job_ex(
    hub: &HubCtx,
    rt: &tokio::runtime::Runtime,
    client: Option<&ChainClient>,
    provider_addr_hex: &str,
    status_url: &str,
    model: &str,
    prompt: &str,
    max_tokens: u32,
    price: u128,
) -> Result<ChannelJobOut, ChannelFail> {
    let no = |m: &str| Err(ChannelFail::NotAttempted(m.to_string()));
    if !hub.enabled {
        return no("hub disabled (GPN_ESCROW_MODE=onchain/off)");
    }
    if price == 0 {
        return no("channel price is zero");
    }
    let Some(provider_pub) = PubKey::parse(provider_addr_hex) else {
        return no("provider address is not a valid sr25519 key");
    };
    if hub.is_unsupported(provider_addr_hex) {
        return no("provider does not speak /channel (recently refused)");
    }
    let Some(endpoint) = channel_endpoint(status_url) else {
        return no("cannot derive the provider's channel endpoint from its status URL");
    };
    // Only pay for a channel open when the provider actually speaks the
    // protocol. Existing channels skip the probe (they already worked), which is
    // the normal case after the first job.
    let have_channel = {
        let reg = hub.registry.lock().unwrap();
        reg.channels
            .get(provider_addr_hex)
            .map(|c| c.state.balance_requestor > 0)
            .unwrap_or(false)
    };
    if !have_channel && !probe_channel_support(status_url) {
        let msg = format!(
            "provider {provider_addr_hex} does not serve /channel/status — staying on-chain (no channel opened)"
        );
        eprintln!("[hub] {msg}");
        hub.mark_unsupported(provider_addr_hex);
        return Err(ChannelFail::NotAttempted(msg));
    }
    let channel_id = match ensure_hub_channel(hub, rt, client, provider_addr_hex, &provider_pub) {
        Some(id) => id,
        None => return no("no usable channel (open/funding failed or channel settled on chain)"),
    };

    // Price the job under the registry lock, then release it for the HTTP call.
    let assignment: Assignment = {
        let mut reg = hub.registry.lock().unwrap();
        let Some(ch) = reg.channels.get_mut(provider_addr_hex) else {
            return no("channel disappeared from the registry");
        };
        if ch.channel_id != channel_id {
            return no("channel id changed under us");
        }
        let job_id = next_job_id();
        let deadline = now_ms().saturating_add(hub.job_deadline_ms);
        match ch.begin_job_for(
            &hub.signer,
            provider_pub,
            job_id,
            model,
            prompt,
            max_tokens,
            700,
            price,
            deadline,
        ) {
            Ok(a) => {
                let _ = reg.save(&hub.path);
                a
            }
            Err(e) => {
                eprintln!("[hub] cannot price job on channel {channel_id}: {e}");
                return Err(ChannelFail::NotAttempted(format!(
                    "cannot price job on channel {channel_id}: {e}"
                )));
            }
        }
    };
    let job_id = assignment.job_id;

    let body = match serde_json::to_string(&assignment) {
        Ok(b) => b,
        Err(e) => {
            expire_pending(hub, provider_addr_hex, job_id);
            return no(&format!("assignment serialization failed: {e}"));
        }
    };
    let http = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(hub.job_deadline_ms.max(5_000)))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            abort_pending(hub, provider_addr_hex, job_id);
            return no(&format!("http client: {e}"));
        }
    };
    let resp = match http.post(&endpoint).body(body).send() {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            let code = r.status().as_u16();
            let msg = format!("provider {provider_addr_hex} /channel/job -> HTTP {code}");
            eprintln!("[hub] {msg}");
            hub.mark_unsupported(provider_addr_hex);
            if (400..500).contains(&code) {
                // Accept-time rejection: `ProviderChannel::accept` refused the
                // assignment, so the provider committed nothing.
                abort_pending(hub, provider_addr_hex, job_id);
                return Err(ChannelFail::NotAttempted(msg));
            }
            // 5xx: the provider may have accepted and then crashed mid-run.
            // Keep the pending entry; do NOT let the caller pay again.
            return Err(ChannelFail::Ambiguous(format!(
                "{msg} (work may have started; not falling back)"
            )));
        }
        Err(e) => {
            let msg = format!("provider {provider_addr_hex} /channel/job unreachable: {e}");
            eprintln!("[hub] {msg} — falling back");
            hub.mark_unsupported(provider_addr_hex);
            abort_pending(hub, provider_addr_hex, job_id);
            return Err(ChannelFail::NotAttempted(msg));
        }
    };
    let result: JobResultMsg = match resp.json() {
        Ok(v) => v,
        Err(e) => {
            // The provider answered 200 and we cannot read it: it may have run
            // the work and committed the state. Ambiguous by construction.
            let msg = format!("provider {provider_addr_hex} returned an unparsable result: {e}");
            eprintln!("[hub] {msg}");
            hub.mark_unsupported(provider_addr_hex);
            return Err(ChannelFail::Ambiguous(format!(
                "{msg} (job {job_id} left pending; it will expire)"
            )));
        }
    };

    let mut reg = hub.registry.lock().unwrap();
    let applied = {
        let Some(ch) = reg.channels.get_mut(provider_addr_hex) else {
            return Err(ChannelFail::Ambiguous("channel vanished before apply".into()));
        };
        match ch.complete_job(&result) {
            Ok(()) => Some((ch.channel_id, ch.state.nonce, ch.state.balance_provider)),
            Err(e) => {
                eprintln!(
                    "[hub] rejected provider result for job {}: {e}",
                    result.job_id
                );
                None
            }
        }
    };
    let Some((channel_id, nonce, balance_provider)) = applied else {
        // We received a signed result we cannot apply. The provider may already
        // have advanced its own state, so the caller must not pay again.
        return Err(ChannelFail::Ambiguous(format!(
            "provider result for job {job_id} rejected; channel state not advanced"
        )));
    };
    let _ = reg.save(&hub.path);
    drop(reg);
    let usage = if result.prompt_tokens > 0 || result.completion_tokens > 0 {
        Some((result.prompt_tokens, result.completion_tokens))
    } else {
        None
    };
    println!(
        "[hub] job {} served off-chain on channel {} (nonce {}, provider balance {})",
        result.job_id, channel_id, nonce, balance_provider
    );
    if !result.ok {
        // The provider signed a *refund*: balances reverted, nonce advanced.
        // No value moved, so the caller may safely try the on-chain path.
        return Err(ChannelFail::NotAttempted(format!(
            "provider refunded job {}: {}",
            result.job_id, result.output
        )));
    }
    Ok(ChannelJobOut {
        channel_id,
        job_id: result.job_id,
        text: result.output,
        usage,
        balance_provider,
    })
}

/// Route one job through the hub channel. `None` = channel path unavailable or
/// failed; see [`try_channel_job_ex`] when the caller needs to know whether
/// falling back is safe (i.e. whether any money could have moved).
#[allow(clippy::too_many_arguments)]
pub fn try_channel_job(
    hub: &HubCtx,
    rt: &tokio::runtime::Runtime,
    client: &ChainClient,
    provider_addr_hex: &str,
    status_url: &str,
    model: &str,
    prompt: &str,
    max_tokens: u32,
    price: u128,
) -> Option<ChannelJobOut> {
    try_channel_job_ex(
        hub,
        rt,
        Some(client),
        provider_addr_hex,
        status_url,
        model,
        prompt,
        max_tokens,
        price,
    )
    .ok()
}


fn expire_pending(hub: &HubCtx, provider: &str, job_id: u64) {
    let mut reg = hub.registry.lock().unwrap();
    if let Some(ch) = reg.channels.get_mut(provider) {
        let _ = ch.expire_pending(job_id);
        let _ = reg.save(&hub.path);
    }
}

/// Drop a pending entry whose assignment provably never reached a provider.
fn abort_pending(hub: &HubCtx, provider: &str, job_id: u64) {
    let mut reg = hub.registry.lock().unwrap();
    if let Some(ch) = reg.channels.get_mut(provider) {
        ch.abort_pending(job_id);
        let _ = reg.save(&hub.path);
    }
}

/// Monotone off-chain job id. Local to this AP (it never goes on chain), but
/// unique per process *and* across restarts via the millisecond component.
fn next_job_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST: AtomicU64 = AtomicU64::new(0);
    let wall = now_ms();
    loop {
        let prev = LAST.load(Ordering::Relaxed);
        let cand = wall.max(prev + 1);
        if LAST
            .compare_exchange(prev, cand, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            return cand;
        }
    }
}

/// Reuse the live channel for this provider, adopting one found on chain, or
/// open it (the single on-chain tx of the whole path).
fn ensure_hub_channel(
    hub: &HubCtx,
    rt: &tokio::runtime::Runtime,
    client: Option<&ChainClient>,
    provider_hex: &str,
    provider_pub: &PubKey,
) -> Option<u64> {
    {
        let reg = hub.registry.lock().unwrap();
        if let Some(ch) = reg.channels.get(provider_hex) {
            if ch.state.balance_requestor > 0 {
                return Some(ch.channel_id);
            }
        }
    }

    // A channel that is not in the registry needs a chain read (and possibly an
    // `open_channel`). Without a chain client there is nothing safe to do.
    let client = client?;
    let ap_pub = hub.signer.public_key();
    let mut id = rt.block_on(client.get_channel_id(&ap_pub, &provider_pub.0));
    if id.is_none() {
        println!(
            "[hub] opening channel → {provider_hex} (deposit={} ttl={} blocks, once)",
            hub.deposit, hub.ttl
        );
        match rt.block_on(client.open_channel(provider_pub.0, hub.deposit, hub.ttl)) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("[hub] open_channel failed: {e}");
                return None;
            }
        }
        // Inclusion is confirmed by submit_signed; the index write is in the
        // same block, but allow a couple of reads for RPC head lag.
        id = rt.block_on(client.get_channel_id(&ap_pub, &provider_pub.0));
        for _ in 0..10 {
            if id.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
            id = rt.block_on(client.get_channel_id(&ap_pub, &provider_pub.0));
        }
    }
    let channel_id = id?;

    let onchain: Option<OnChainChannel> = rt.block_on(client.get_channel(channel_id));
    let (deposit, state) = match onchain {
        Some(c) if c.is_open => {
            let deposit = c
                .deposit
                .max(c.balance_requestor.saturating_add(c.balance_provider));
            (
                deposit,
                ChannelState {
                    balance_requestor: c.balance_requestor,
                    balance_provider: c.balance_provider,
                    nonce: c.nonce,
                },
            )
        }
        Some(_) => {
            eprintln!("[hub] channel {channel_id} is settled on chain; will reopen next time");
            return None;
        }
        None => (hub.deposit, ChannelState::new(hub.deposit)),
    };

    let mut reg = hub.registry.lock().unwrap();
    reg.channels.insert(
        provider_hex.to_string(),
        HubChannel::with_state(channel_id, deposit, state),
    );
    let _ = reg.save(&hub.path);
    println!("[hub] channel {channel_id} ready for {provider_hex} (deposit {deposit})");
    Some(channel_id)
}

/// Periodic cooperative settlement: when the provider's off-chain balance (or
/// the update count) crosses the configured threshold, close on-chain with the
/// latest mutually-signed state and drop the channel from the registry. The next
/// job opens a fresh one.
pub fn spawn_settler(hub: Arc<HubCtx>, rt: Arc<tokio::runtime::Runtime>, client: Arc<ChainClient>) {
    if !hub.enabled {
        return;
    }
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(60));
        let due: Vec<(String, crate::hub_channel::CloseMaterial)> = {
            let reg = hub.registry.lock().unwrap();
            reg.channels
                .iter()
                .filter(|(_, ch)| {
                    ch.pending_len() == 0 && ch.should_close(hub.close_at, hub.close_nonce)
                })
                .filter_map(|(k, ch)| ch.close_material(&hub.signer).ok().map(|m| (k.clone(), m)))
                .collect()
        };
        for (provider, m) in due {
            println!(
                "[hub] settling channel {} (nonce {}, provider {})",
                m.channel_id, m.state.nonce, m.state.balance_provider
            );
            let ok = rt
                .block_on(client.close_channel(
                    m.channel_id,
                    m.state.balance_requestor,
                    m.state.balance_provider,
                    m.state.nonce,
                    m.sig_requestor.0,
                    m.sig_provider.0,
                ))
                .is_ok();
            if ok {
                let mut reg = hub.registry.lock().unwrap();
                reg.channels.remove(&provider);
                let _ = reg.save(&hub.path);
                println!("[hub] channel {} settled on chain", m.channel_id);
            } else {
                eprintln!("[hub] close_channel {} failed (will retry)", m.channel_id);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_endpoint_derives_from_status_url() {
        assert_eq!(
            channel_endpoint("http://127.0.0.1:8093/status").as_deref(),
            Some("http://127.0.0.1:8094/channel/job")
        );
        assert_eq!(
            channel_endpoint("http://127.0.0.1:8093").as_deref(),
            Some("http://127.0.0.1:8094/channel/job")
        );
        // Any port works: the channel server is heartbeat_port + 1.
        assert_eq!(
            channel_endpoint("http://host:9090/status").as_deref(),
            Some("http://host:9091/channel/job")
        );
        assert_eq!(channel_endpoint("http://host/status"), None);
    }

    #[test]
    fn job_ids_are_strictly_increasing() {
        let a = next_job_id();
        let b = next_job_id();
        assert!(b > a);
    }
}
