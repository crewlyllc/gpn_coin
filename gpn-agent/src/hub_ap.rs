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

pub struct ChannelJobOut {
    pub channel_id: u64,
    pub job_id: u64,
    pub text: String,
    pub usage: Option<(u32, u32)>,
    pub balance_provider: u128,
}

/// Route one job through the hub channel. `None` = channel path unavailable or
/// failed; the caller must fall back to the on-chain escrow path (no state was
/// left half-applied: a failed request only leaves a pending entry, which the
/// next sweep expires).
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
    if !hub.enabled || price == 0 {
        return None;
    }
    let provider_pub = PubKey::parse(provider_addr_hex)?;
    if hub.is_unsupported(provider_addr_hex) {
        return None;
    }
    let endpoint = channel_endpoint(status_url)?;
    let channel_id = ensure_hub_channel(hub, rt, client, provider_addr_hex, &provider_pub)?;

    // Price the job under the registry lock, then release it for the HTTP call.
    let assignment: Assignment = {
        let mut reg = hub.registry.lock().unwrap();
        let ch = reg.channels.get_mut(provider_addr_hex)?;
        if ch.channel_id != channel_id {
            return None;
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
                return None;
            }
        }
    };

    let body = serde_json::to_string(&assignment).ok()?;
    let http = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(hub.job_deadline_ms.max(5_000)))
        .build()
    {
        Ok(c) => c,
        Err(_) => return None,
    };
    let resp = match http.post(&endpoint).body(body).send() {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            eprintln!(
                "[hub] provider {provider_addr_hex} /channel/job -> HTTP {} — falling back",
                r.status()
            );
            hub.mark_unsupported(provider_addr_hex);
            expire_pending(hub, provider_addr_hex, assignment.job_id);
            return None;
        }
        Err(e) => {
            eprintln!(
                "[hub] provider {provider_addr_hex} /channel/job unreachable: {e} — falling back"
            );
            hub.mark_unsupported(provider_addr_hex);
            expire_pending(hub, provider_addr_hex, assignment.job_id);
            return None;
        }
    };
    let result: JobResultMsg = match resp.json() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[hub] provider {provider_addr_hex} returned an unparsable result: {e}");
            hub.mark_unsupported(provider_addr_hex);
            expire_pending(hub, provider_addr_hex, assignment.job_id);
            return None;
        }
    };

    let mut reg = hub.registry.lock().unwrap();
    let applied = {
        let ch = reg.channels.get_mut(provider_addr_hex)?;
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
    }?;
    let _ = reg.save(&hub.path);
    drop(reg);
    let (channel_id, nonce, balance_provider) = applied;
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
        return None; // refunded: let the caller surface the error / retry
    }
    Some(ChannelJobOut {
        channel_id,
        job_id: result.job_id,
        text: result.output,
        usage,
        balance_provider,
    })
}

fn expire_pending(hub: &HubCtx, provider: &str, job_id: u64) {
    let mut reg = hub.registry.lock().unwrap();
    if let Some(ch) = reg.channels.get_mut(provider) {
        let _ = ch.expire_pending(job_id);
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
    client: &ChainClient,
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
