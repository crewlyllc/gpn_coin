//! gpn_provider_status — a tiny HTTP heartbeat for a provider, polled by the
//! access-point scheduler to learn capabilities + load + latency.
//!
//! `GET /status` → `{ address, gflops, models[], capabilities[], can_pull, active, capacity, vram_mb, ram_mb, gpu_class, tflops, disk_free_mb, disk_total_mb, disk_free_pct, peer_id?, multiaddrs[]?, p2p_port?, listen_addrs[]? }`
//!
//! `capabilities[]` lists the JOB TYPES this node serves — `LLMInference`,
//! `ImageGeneration`, `VideoGeneration`, `MLInference`, `FineTuning`. A GPN
//! node is equipped to serve **every** job type by design ("one GPU, one job"
//! means one job *at a time* — mutual exclusion — NOT a restriction on job
//! types). Advertised so the scheduler routes every kind of job here. Override
//! with `--capabilities` (env `CAPABILITIES`); a constrained box that opts a
//! provider out narrows the list.
//!
//! `vram_mb`/`ram_mb` advertise this box's hardware so the scheduler can gate a
//! job that declares a VRAM/RAM floor (item 14 — e.g. video needs a big-VRAM box).
//! They are auto-detected (nvidia-smi / /proc/meminfo) and can be overridden with
//! `--vram-mb`/`--ram-mb` (env `VRAM_MB`/`RAM_MB`); 0 = unknown (not advertised).
//!
//! `gpu_class` (`discrete`|`integrated`|`unknown`) + `tflops` advertise whether
//! this box has a real (discrete) GPU (item 19 — a CPU/APU integrated GPU must
//! not earn unless it clears an FP32-TFLOPS floor). Auto-detected (nvidia-smi /
//! lspci on Linux, Win32_VideoController on Windows); override with
//! `--gpu-class`/`--tflops` (env `GPU_CLASS`/`TFLOPS`); tflops 0 = unknown.
//!
//! `models[]` is read LIVE from the local Ollama (`GET /api/tags`), so the network
//! always sees what this machine actually has. `can_pull` = Ollama is reachable,
//! i.e. this provider could download a requested model on demand.
//!
//! `peer_id` / `multiaddrs` / `p2p_port` (peer-id publication) let the public
//! directory (`https://gpncoin.ai/seeds.json`) turn this provider into a
//! **dialable** libp2p peer instead of an informational HTTP endpoint. The peer
//! id + listen addresses are read from the local node over JSON-RPC
//! (`system_localPeerId` / `system_localListenAddresses`) and cached.
//!
//! # Publishing a dialable address
//!
//! `multiaddrs` must contain **only** endpoints a stranger can dial: never
//! loopback, RFC1918, CGNAT (`100.64/10`) or link-local. The public endpoint is
//! chosen by the same evidence ladder the node's dialability gate uses, in
//! descending order of trust:
//!
//! 1. `GPN_PUBLIC_ADDR` / `--public-addr` — the operator says so.
//! 2. The node's own dialability state file (`GPN_DIALABILITY_STATE`, default
//!    `$HOME/.gpn/dialability.json`) — written by `gpn-node` after the gate
//!    either observed an inbound peer or mapped a port (UPnP-IGD / NAT-PMP).
//! 3. A public listen address — the classic VPS case (bound to a public IP).
//! 4. A STUN-mapped address **on the listen port** (port-preserving NAT).
//!
//! If no rung yields anything, `multiaddrs` is `[]` and every rejected rung is
//! logged with its reason, so an operator can see exactly what to fix instead of
//! guessing why the directory never lists their box.
//!
//! # listen_addrs are diagnostic, never dialable claims
//!
//! Current litep2p builds report `system_localListenAddresses` already carrying
//! `/p2p/<peer_id>` — and some report it **doubled** (`/p2p/<id>/p2p/<id>`,
//! upstream cosmetic bug). Relaying that verbatim made the access point's
//! sanitizer (which correctly requires exactly one trailing `/p2p/<own id>`)
//! reject every address, so the provider looked peer-less. Listen addresses are
//! therefore normalised to `<proto>/<host>/tcp/<port>` before publication and the
//! peer id is appended exactly once, by us.
//!
//! Usage:
//!   gpn_provider_status --listen <ip:port> --address <ss58> --gflops <n>
//!                       [--models <m1,m2>] [--capacity <n>] [--active <n>]
//!                       [--ollama http://127.0.0.1:11434]
//!                       [--capabilities <LLMInference,ImageGeneration,…>]
//!                       [--endpoint ws://127.0.0.1:9944] [--public-addr <ip:port>]
//! Env: LISTEN, ADDRESS, GFLOPS, MODELS, CAPACITY, ACTIVE, GPN_OLLAMA, CAPABILITIES,
//!      GPN_ENDPOINT, GPN_PUBLIC_ADDR

use gpn_agent::disk::{disk_space, LOW_DISK_PERCENT};
use gpn_agent::gpu::{detect_gpu_capability, detect_vram_mb};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

use tiny_http::{Header, Method, Response, Server};

fn opt(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

/// How long a model-inventory answer is reused. The access point polls
/// `/status` every ~5 s, so an uncached live probe here meant one blocking
/// `ollama /api/tags` GET every 5 s forever (~12 req/min of pure noise) — the
/// same class as item #48 in `gpn_llm_provider`. Short enough that a freshly
/// pulled model still appears almost immediately.
const MODELS_TTL: Duration = Duration::from_secs(10);

/// How long a peer-id snapshot is reused. The peer id never changes for a
/// running node, but the listen addresses can (restart on a different port), so
/// refresh occasionally instead of once at boot. A failure is retried sooner:
/// the node RPC may simply not be up yet when the heartbeat starts.
const P2P_TTL: Duration = Duration::from_secs(60);
const P2P_RETRY_TTL: Duration = Duration::from_secs(10);

/// How long a STUN result is reused. A NAT mapping can change (router reboot,
/// lease expiry); a few minutes is cheap and fresh enough.
const STUN_TTL: Duration = Duration::from_secs(300);

/// Default STUN server for the last rung of the evidence ladder.
const DEFAULT_STUN_SERVER: &str = "stun.l.google.com:19302";

/// What the directory needs to publish this provider as a dialable peer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct P2pInfo {
    peer_id: String,
    /// Dialable, public multiaddrs (already carrying `/p2p/<peer_id>`).
    multiaddrs: Vec<String>,
    /// The libp2p TCP port peers can dial (advertised even when the listen
    /// address itself is `0.0.0.0`, so the AP can combine the peer id + port
    /// with the host it already reached us on).
    p2p_port: Option<u16>,
    /// `system_localListenAddresses`, normalised to base addresses (no
    /// `/p2p/…` suffix) for diagnostics.
    listen_addrs: Vec<String>,
    /// Which rung of the evidence ladder produced `multiaddrs` ("none" when
    /// empty) — surfaced in `/status` so an operator can see WHY.
    address_source: String,
}

/// Convert a `ws(s)://` / `http(s)://` endpoint to plain HTTP for JSON-RPC and
/// issue one call. Any failure is `None` — callers treat that as "unknown".
fn rpc_call(endpoint: &str, method: &str, params: Value) -> Option<Value> {
    let http_url = endpoint
        .replace("ws://", "http://")
        .replace("wss://", "https://");
    let body = json!({"jsonrpc":"2.0","id":1,"method":method,"params":params});
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(1500))
        .build()
        .ok()?;
    let v: Value = client
        .post(&http_url)
        .json(&body)
        .send()
        .ok()?
        .json()
        .ok()?;
    v.get("result").cloned()
}

// ─── Attested compute (queue #65) ───────────────────────────────────────────
//
// `/status.gflops` is what an access point routes on (`min_gflops`), while the
// CHAIN enforces `attested_gflops >= min_gflops` when a provider accepts a job.
// Those two numbers used to be independent: the heartbeat advertised a
// hand-typed `--gflops` (default **0**) that nothing reconciled with the
// attestation. So box1 advertised 0 GFLOPS — unroutable for any gated job —
// while the chain held a real attestation for it; and the reverse (advertising
// more than was ever attested) only shows up later as an `accept_job` rejection
// that strands the job.
//
// Resolve it from the chain instead, and never advertise more than the chain
// will accept: `min(operator, attested)` when both are known.

/// How long a chain-read attestation stays authoritative in `/status`.
const ATTEST_TTL: Duration = Duration::from_secs(60);
/// Retry sooner when the last read failed (chain down / RPC busy).
const ATTEST_RETRY_TTL: Duration = Duration::from_secs(30);

fn attest_cache() -> &'static std::sync::Mutex<Option<(Option<u64>, Instant)>> {
    static C: std::sync::OnceLock<std::sync::Mutex<Option<(Option<u64>, Instant)>>> =
        std::sync::OnceLock::new();
    C.get_or_init(|| std::sync::Mutex::new(None))
}

/// ss58 or `0x`-hex → 32-byte account id.
fn account_pubkey(address: &str) -> Option<[u8; 32]> {
    use sp_core::crypto::Ss58Codec;
    let a = address.trim();
    if let Some(hexpart) = a.strip_prefix("0x") {
        let bytes = hex::decode(hexpart).ok()?;
        return bytes.as_slice().try_into().ok();
    }
    let id = sp_core::crypto::AccountId32::from_ss58check(a).ok()?;
    Some(id.into())
}

/// Storage key for `GPUAttestation::Attestations[who]`.
fn attestation_key(pubkey: &[u8; 32]) -> String {
    let mut k = Vec::with_capacity(64);
    k.extend_from_slice(&sp_core::hashing::twox_128(b"GPUAttestation"));
    k.extend_from_slice(&sp_core::hashing::twox_128(b"Attestations"));
    k.extend_from_slice(&sp_core::hashing::blake2_128(pubkey));
    k.extend_from_slice(pubkey); // `blake2_128_concat` = hash ++ key
    format!("0x{}", hex::encode(k))
}

/// `AttestationRecord` → `compute_gflops`.
///
/// Fixed-width SCALE prefix, so the offset is stable:
/// `submitted_at u32 (0..4)` · `gpu_info_hash [u8;32] (4..36)` ·
/// `challenge_response [u8;32] (36..68)` · `gpu_count u8 (68)` ·
/// `compute_gflops u64 (69..77)`.
fn decode_attested_gflops(bytes: &[u8]) -> Option<u64> {
    if bytes.len() < 77 {
        return None;
    }
    let mut b = [0u8; 8];
    b.copy_from_slice(&bytes[69..77]);
    Some(u64::from_le_bytes(b))
}

/// The provider's on-chain attested GFLOPS (None = unreadable / never attested).
/// Cached briefly: `/status` is polled every 5 s by the access point, and the
/// chain value only changes when the provider re-attests.
fn attested_gflops(endpoint: &str, address: &str) -> Option<u64> {
    if let Ok(guard) = attest_cache().lock() {
        if let Some((v, at)) = guard.as_ref() {
            let ttl = if v.is_some() {
                ATTEST_TTL
            } else {
                ATTEST_RETRY_TTL
            };
            if at.elapsed() < ttl {
                return *v;
            }
        }
    }
    let answer = account_pubkey(address).and_then(|pk| {
        rpc_call(
            endpoint,
            "state_getStorage",
            json!([attestation_key(&pk)]),
        )
        .and_then(|v| v.as_str().and_then(|s| hex::decode(s.trim_start_matches("0x")).ok()))
        .and_then(|b| decode_attested_gflops(&b))
    });
    if let Ok(mut guard) = attest_cache().lock() {
        *guard = Some((answer, Instant::now()));
    }
    answer
}

/// What to advertise: never more than the chain will accept, and never nothing
/// when either source knows a real number.
fn resolve_advertised_gflops(operator: u64, attested: Option<u64>) -> (u64, &'static str) {
    match (operator > 0, attested.filter(|g| *g > 0)) {
        (true, Some(a)) => (operator.min(a), "chain attestation (min with --gflops)"),
        (false, Some(a)) => (a, "chain attestation"),
        (true, None) => (operator, "--gflops (chain attestation unreadable)"),
        (false, None) => (0, "none"),
    }
}

/// Accept only a plausible libp2p peer id (base58-ish, ed25519 `12D3Koo…` or
/// legacy multihash `Qm…`). Anything else is dropped rather than relayed.
fn normalize_peer_id(raw: &str) -> Option<String> {
    let s = raw.trim();
    if !(40..=64).contains(&s.len()) {
        return None;
    }
    if !s.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    if s.starts_with("12D3Koo") || s.starts_with("Qm") {
        Some(s.to_string())
    } else {
        None
    }
}

fn is_public_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_unspecified()
                // 100.64.0.0/10 carrier-grade NAT is not globally routable.
                || (v4.octets()[0] == 100 && (64..=127).contains(&v4.octets()[1])))
        }
        std::net::IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // fc00::/7 unique local, fe80::/10 link local
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80)
        }
    }
}

/// `/ip4/1.2.3.4/tcp/30333` | `/dns4/node.example/tcp/30333` | `1.2.3.4:30333`
/// → a base multiaddr (no peer id). Returns `None` for anything that is not a
/// single host + TCP port.
fn base_multiaddr(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if s.starts_with('/') {
        // expected /<proto>/<host>/tcp/<port>
        let parts: Vec<&str> = s.split('/').filter(|p| !p.is_empty()).collect();
        if parts.len() != 4 || parts[2] != "tcp" {
            return None;
        }
        if !matches!(parts[0], "ip4" | "ip6" | "dns" | "dns4" | "dns6") {
            return None;
        }
        let port: u32 = parts[3].parse().ok()?;
        if !(1..=65535).contains(&port) {
            return None;
        }
        return Some(format!("/{}/{}/tcp/{port}", parts[0], parts[1]));
    }
    // bare `host:port` (optionally with a scheme — stripped off)
    let text = s.rsplit_once("://").map(|(_, rest)| rest).unwrap_or(s);
    let authority = text.split(['/', '?', '#']).next()?;
    let host_port = authority
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(authority);
    let (host, port) = if let Some(rest) = host_port.strip_prefix('[') {
        let (h, tail) = rest.split_once(']')?;
        (h, tail.strip_prefix(':')?)
    } else {
        host_port.rsplit_once(':')?
    };
    let port: u32 = port.parse().ok()?;
    if host.is_empty() || !(1..=65535).contains(&port) {
        return None;
    }
    let kind = if host.parse::<std::net::Ipv4Addr>().is_ok() {
        "ip4"
    } else if host.parse::<std::net::Ipv6Addr>().is_ok() {
        "ip6"
    } else {
        "dns4"
    };
    Some(format!("/{kind}/{}/tcp/{port}", host.to_lowercase()))
}

fn port_of(multiaddr: &str) -> Option<u16> {
    let (_, rest) = multiaddr.split_once("/tcp/")?;
    let port: u32 = rest.split('/').next()?.parse().ok()?;
    if (1..=65535).contains(&port) {
        Some(port as u16)
    } else {
        None
    }
}

/// Normalise one `system_localListenAddresses` entry to a base address, dropping
/// any `/p2p/<peer-id>` suffix the node already appended (litep2p does, and
/// some builds emit it **twice**). Returns `None` for an address we cannot
/// express as `<proto>/<host>/tcp/<port>` (e.g. a `/ws` transport or WebRTC).
fn base_of_listen_addr(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    // Everything from the first `/p2p/` on is identity, not transport.
    let head = s.split("/p2p/").next().unwrap_or(s);
    base_multiaddr(head)
}

/// True when the address carries the doubled `/p2p/<id>/p2p/<id>` suffix that
/// upstream litep2p emits. Used only to log the upstream bug once.
fn has_doubled_p2p(raw: &str) -> bool {
    raw.matches("/p2p/").count() > 1
}

/// Which rung of the evidence ladder produced the published endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddrSource {
    /// `GPN_PUBLIC_ADDR` / `--public-addr`.
    Configured,
    /// The node's dialability state file (inbound peer / port mapping).
    NodeEvidence,
    /// A listen address bound directly to a public IP (the VPS case).
    PublicListen,
    /// A STUN binding on the listen port (port-preserving NAT).
    Stun,
    /// Nothing dialable — publish `[]`.
    None,
}

impl AddrSource {
    fn label(self) -> &'static str {
        match self {
            AddrSource::Configured => "GPN_PUBLIC_ADDR",
            AddrSource::NodeEvidence => "node-dialability-state",
            AddrSource::PublicListen => "public-listen-address",
            AddrSource::Stun => "stun-mapped-address",
            AddrSource::None => "none",
        }
    }
}

/// A validated public base address + where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PublicBase {
    base: String,
    source: AddrSource,
}

/// Reject a base whose host is not globally routable. `/dns*` names are
/// accepted as-is (we cannot classify a name without resolving it, and the
/// gate on the node side already checked it).
fn base_is_dialable(base: &str) -> bool {
    let parts: Vec<&str> = base.split('/').filter(|p| !p.is_empty()).collect();
    match parts.first().copied() {
        Some("ip4") | Some("ip6") => match parts.get(1).and_then(|h| h.parse().ok()) {
            Some(ip) => is_public_ip(ip),
            None => false,
        },
        Some("dns") | Some("dns4") | Some("dns6") => true,
        _ => false,
    }
}

/// Pick the public base address from the evidence ladder.
///
/// Pure so it is unit-testable: the caller supplies the node's state-file
/// endpoint and the STUN result (both `None` when unavailable). `notes`
/// accumulates one human-readable line per rung that was considered and
/// rejected, so the operator sees why nothing was published.
fn choose_public_base(
    configured: Option<&str>,
    listen_addrs: &[String],
    node_evidence: Option<&str>,
    stun: Option<&str>,
    notes: &mut Vec<String>,
) -> Option<PublicBase> {
    // 1. Operator-configured address.
    if let Some(raw) = configured {
        match base_multiaddr(raw) {
            Some(base) if base_is_dialable(&base) => {
                return Some(PublicBase {
                    base,
                    source: AddrSource::Configured,
                })
            }
            _ => notes.push(format!(
                "GPN_PUBLIC_ADDR {raw:?} is not a public <proto>/<host>/tcp/<port> address — ignoring it"
            )),
        }
    } else {
        notes.push("GPN_PUBLIC_ADDR unset (no operator-declared endpoint)".into());
    }

    // 2. The node's own dialability verdict (inbound peer observed, or a
    //    UPnP-IGD/NAT-PMP mapping the router confirmed).
    if let Some(raw) = node_evidence {
        match base_multiaddr(raw) {
            Some(base) if base_is_dialable(&base) => {
                return Some(PublicBase {
                    base,
                    source: AddrSource::NodeEvidence,
                })
            }
            _ => notes.push(format!(
                "node dialability state {raw:?} is not a public address — ignoring it"
            )),
        }
    } else {
        notes.push(
            "no node dialability state (node never observed an inbound peer or mapped a port)"
                .into(),
        );
    }

    // 3. A listen address on a public IP.
    let mut saw_private = Vec::new();
    for ma in listen_addrs {
        let Some(base) = base_of_listen_addr(ma) else {
            notes.push(format!("listen address {ma:?} is not a dialable TCP base"));
            continue;
        };
        if base_is_dialable(&base) {
            return Some(PublicBase {
                base,
                source: AddrSource::PublicListen,
            });
        }
        saw_private.push(base);
    }
    if !saw_private.is_empty() {
        notes.push(format!(
            "node listens only on non-routable addresses ({}); set GPN_PUBLIC_ADDR to the forward",
            saw_private.join(", ")
        ));
    }

    // 4. STUN mapping on the listen port.
    if let Some(raw) = stun {
        match base_multiaddr(raw) {
            Some(base) if base_is_dialable(&base) => {
                return Some(PublicBase {
                    base,
                    source: AddrSource::Stun,
                })
            }
            _ => notes.push(format!(
                "STUN mapping {raw:?} is not usable (private/port-rewritten)"
            )),
        }
    } else {
        notes.push("STUN probing unavailable or not port-preserving".into());
    }

    None
}

/// Build the published peer information. `public_addr` is authoritative when
/// configured (a NATed box listening on `0.0.0.0` cannot infer its own public
/// IP); otherwise the evidence ladder in [`choose_public_base`] decides.
fn build_p2p_info(
    peer_id: String,
    public_addr: Option<&str>,
    listen_addrs: Vec<String>,
) -> P2pInfo {
    build_p2p_info_with(peer_id, public_addr, listen_addrs, None, None)
}

/// [`build_p2p_info`] with the extra evidence rungs (node state file + STUN).
fn build_p2p_info_with(
    peer_id: String,
    public_addr: Option<&str>,
    listen_addrs: Vec<String>,
    node_evidence: Option<&str>,
    stun: Option<&str>,
) -> P2pInfo {
    // Normalise BEFORE anything is published: the node may hand us
    // `/…/tcp/30333/p2p/<id>` (or even `/p2p/<id>/p2p/<id>`). Re-emitting that
    // verbatim is what made every access point reject our addresses.
    let mut clean_listen: Vec<String> = Vec::new();
    let mut doubled = false;
    for ma in &listen_addrs {
        let norm = base_of_listen_addr(ma).unwrap_or_else(|| {
            // Keep an unparseable listener for diagnostics, minus identity.
            ma.split("/p2p/").next().unwrap_or(ma).trim().to_string()
        });
        if has_doubled_p2p(ma) {
            doubled = true;
        }
        if !norm.is_empty() && !clean_listen.contains(&norm) {
            clean_listen.push(norm);
        }
    }
    if doubled {
        eprintln!(
            "[p2p] node RPC reported a doubled /p2p/<id>/p2p/<id> listen address (upstream \
             litep2p bug) — normalised before publication"
        );
    }

    let mut notes: Vec<String> = Vec::new();
    let chosen = choose_public_base(public_addr, &clean_listen, node_evidence, stun, &mut notes);
    let (bases, source) = match &chosen {
        Some(p) => (vec![p.base.clone()], p.source),
        None => (Vec::new(), AddrSource::None),
    };

    let mut multiaddrs: Vec<String> = bases
        .into_iter()
        .map(|b| format!("{b}/p2p/{peer_id}"))
        .collect();
    multiaddrs.sort();
    multiaddrs.dedup();

    let p2p_port = multiaddrs
        .first()
        .and_then(|m| port_of(m))
        .or_else(|| clean_listen.iter().find_map(|m| port_of(m)));

    // Explain an empty publication once per change — an operator seeing
    // `multiaddrs: []` in the directory must not have to guess.
    if multiaddrs.is_empty() {
        eprintln!(
            "[p2p] publishing NO public address for peer {peer_id} — peers cannot dial this box:"
        );
        for n in &notes {
            eprintln!("[p2p]   - {n}");
        }
        eprintln!(
            "[p2p]   fix: set GPN_PUBLIC_ADDR=/ip4/<wan-ip>/tcp/<port> (or enable UPnP) and restart \
             the heartbeat"
        );
    }

    P2pInfo {
        peer_id,
        multiaddrs,
        p2p_port,
        listen_addrs: clean_listen,
        address_source: source.label().to_string(),
    }
}

/// Read peer id + listen addresses from the node RPC. `None` when the node is
/// unreachable or answers something unexpected.
fn read_p2p_info(
    endpoint: &str,
    public_addr: Option<&str>,
    node_evidence: Option<&str>,
    stun: Option<&str>,
) -> Option<P2pInfo> {
    p2p_info_from_rpc_evidence(
        &|method| rpc_call(endpoint, method, json!([])),
        public_addr,
        node_evidence,
        stun,
    )
}

/// RPC-agnostic core of [`read_p2p_info`] — injectable so tests can mock the
/// node without a socket.
fn p2p_info_from_rpc(
    rpc: &dyn Fn(&str) -> Option<Value>,
    public_addr: Option<&str>,
) -> Option<P2pInfo> {
    p2p_info_from_rpc_evidence(rpc, public_addr, None, None)
}

fn p2p_info_from_rpc_evidence(
    rpc: &dyn Fn(&str) -> Option<Value>,
    public_addr: Option<&str>,
    node_evidence: Option<&str>,
    stun: Option<&str>,
) -> Option<P2pInfo> {
    let peer_id = rpc("system_localPeerId").and_then(|v| v.as_str().map(normalize_peer_id))??;
    let listen_addrs = rpc("system_localListenAddresses")
        .and_then(|v| {
            v.as_array().map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
        })
        .unwrap_or_default();
    Some(build_p2p_info_with(
        peer_id,
        public_addr,
        listen_addrs,
        node_evidence,
        stun,
    ))
}

/// Attach the peer-id fields to a `/status` body. Omitted entirely when there
/// is nothing to publish, so the shape stays backwards compatible.
fn attach_p2p(body: &mut Value, info: Option<&P2pInfo>) {
    if let Some(p2p) = info {
        body["peer_id"] = json!(p2p.peer_id);
        body["multiaddrs"] = json!(p2p.multiaddrs);
        body["p2p_port"] = json!(p2p.p2p_port);
        body["listen_addrs"] = json!(p2p.listen_addrs);
        body["address_source"] = json!(p2p.address_source);
    }
}

// ── Evidence rungs: node state file + STUN ─────────────────────────────

/// Read the public endpoint the node's dialability gate settled on.
///
/// `gpn-node` writes `{ base_path }/dialability.json` (and honours
/// `GPN_DIALABILITY_STATE`) after the gate passes or a port mapping succeeds.
/// Accepted shapes: `{"public_endpoint": "/ip4/…/tcp/…"}`, `{"public_addr": …}`
/// or `{"multiaddrs": ["…"]}`. Anything unreadable is simply absent — the
/// heartbeat never fails because the node's state file is missing.
fn read_dialability_state() -> Option<String> {
    let path = std::env::var("GPN_DIALABILITY_STATE").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        format!("{home}/.gpn/dialability.json")
    });
    let text = std::fs::read_to_string(&path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    for key in ["public_endpoint", "public_addr", "public_address"] {
        if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
            if !s.trim().is_empty() {
                return Some(s.trim().to_string());
            }
        }
    }
    v.get("multiaddrs")
        .and_then(|x| x.as_array())
        .and_then(|a| a.iter().filter_map(|s| s.as_str()).next())
        .map(str::to_string)
}

/// One STUN binding request from a UDP socket bound to `port` (the node's p2p
/// port — UDP and TCP ports are independent, so this does not disturb the
/// node's TCP listener). Returns the mapped `(ip, port)` the NAT reports.
///
/// Only the standard Binding Request + XOR-MAPPED-ADDRESS path is implemented:
/// that is all the ladder needs, and pulling in a STUN crate for one packet is
/// not worth it.
fn stun_binding(port: u16, server: &str, timeout: Duration) -> Option<(std::net::IpAddr, u16)> {
    use std::net::{IpAddr, Ipv4Addr, ToSocketAddrs, UdpSocket};
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port)).ok()?;
    sock.set_read_timeout(Some(timeout)).ok()?;
    let addr = server.to_socket_addrs().ok()?.find(|a| a.is_ipv4())?;

    let mut req = [0u8; 20];
    req[0..2].copy_from_slice(&0x0001u16.to_be_bytes()); // Binding Request
    req[2..4].copy_from_slice(&0u16.to_be_bytes()); // length
    req[4..8].copy_from_slice(&0x2112_A442u32.to_be_bytes()); // magic cookie
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        ^ ((port as u64) << 32)
        ^ (&req as *const _ as u64);
    let mut x = seed | 1;
    for b in req[8..20].iter_mut() {
        // xorshift64 — a transaction id needs unpredictability, not crypto.
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = (x & 0xff) as u8;
    }

    sock.send_to(&req, addr).ok()?;
    let mut buf = [0u8; 512];
    let (n, _) = sock.recv_from(&mut buf).ok()?;
    let msg = &buf[..n];
    if msg.len() < 20 || u16::from_be_bytes([msg[0], msg[1]]) != 0x0101 {
        return None;
    }
    if u32::from_be_bytes([msg[4], msg[5], msg[6], msg[7]]) != 0x2112_A442 {
        return None;
    }
    let attrs = &msg[20..];
    let mut i = 0usize;
    while i + 4 <= attrs.len() {
        let kind = u16::from_be_bytes([attrs[i], attrs[i + 1]]);
        let len = u16::from_be_bytes([attrs[i + 2], attrs[i + 3]]) as usize;
        let val = attrs.get(i + 4..i + 4 + len)?;
        // 0x0020 = XOR-MAPPED-ADDRESS (0x0001 is the obsolete MAPPED-ADDRESS).
        if (kind == 0x0020 || kind == 0x0001) && val.len() >= 8 {
            let xor = kind == 0x0020;
            let fam = val[1];
            let raw_port = u16::from_be_bytes([val[2], val[3]]);
            let mapped_port = if xor { raw_port ^ 0x2112 } else { raw_port };
            if fam == 0x01 {
                let mut o = [val[4], val[5], val[6], val[7]];
                if xor {
                    let c = 0x2112_A442u32.to_be_bytes();
                    for k in 0..4 {
                        o[k] ^= c[k];
                    }
                }
                return Some((IpAddr::V4(Ipv4Addr::from(o)), mapped_port));
            }
            return None; // IPv6 NAT is not a supported deployment
        }
        i += 4 + len + ((4 - (len % 4)) % 4);
    }
    None
}

/// Cached STUN evidence as a base multiaddr, or `None`.
///
/// A STUN answer is only usable as a *published* address when the NAT is
/// port-preserving (mapped port == listen port): otherwise peers must dial a
/// different port than we listen on, and only the router (UPnP/NAT-PMP) or the
/// operator can tell us which one. In that case we log the observed mapping and
/// publish nothing.
fn stun_evidence(port: Option<u16>) -> Option<String> {
    if std::env::var("GPN_STUN")
        .map(|v| v.eq_ignore_ascii_case("off"))
        .unwrap_or(false)
    {
        return None;
    }
    let port = port?;
    static CACHE: std::sync::Mutex<Option<(Instant, Option<String>)>> = std::sync::Mutex::new(None);
    if let Ok(g) = CACHE.lock() {
        if let Some((at, val)) = g.as_ref() {
            if at.elapsed() < STUN_TTL {
                return val.clone();
            }
        }
    }
    let server =
        std::env::var("GPN_STUN_SERVER").unwrap_or_else(|_| DEFAULT_STUN_SERVER.to_string());
    let answer = match stun_binding(port, &server, Duration::from_millis(1200)) {
        Some((ip, mapped_port)) => {
            if !is_public_ip(ip) {
                eprintln!("[p2p] STUN via {server} reports {ip} (not public) — no forward works");
                None
            } else if mapped_port != port {
                eprintln!(
                    "[p2p] STUN via {server} maps to {ip}:{mapped_port} but we listen on {port} \
                     (port-rewriting NAT) — set GPN_PUBLIC_ADDR=/ip4/{ip}/tcp/{mapped_port}"
                );
                None
            } else {
                Some(format!("/ip4/{ip}/tcp/{port}"))
            }
        }
        None => {
            eprintln!("[p2p] STUN via {server} did not answer — skipping that rung");
            None
        }
    };
    if let Ok(mut g) = CACHE.lock() {
        *g = Some((Instant::now(), answer.clone()));
    }
    answer
}

fn p2p_cache() -> &'static std::sync::Mutex<Option<(Instant, Option<P2pInfo>)>> {
    static C: std::sync::OnceLock<std::sync::Mutex<Option<(Instant, Option<P2pInfo>)>>> =
        std::sync::OnceLock::new();
    C.get_or_init(|| std::sync::Mutex::new(None))
}

/// Cached [`read_p2p_info`]. Never blocks the heartbeat for long: the RPC calls
/// are time-bounded at 1.5 s and a miss (node not up yet) is retried in 10 s.
fn p2p_info(endpoint: &str, public_addr: Option<&str>) -> Option<P2pInfo> {
    if let Ok(guard) = p2p_cache().lock() {
        if let Some((at, info)) = guard.as_ref() {
            let ttl = if info.is_some() {
                P2P_TTL
            } else {
                P2P_RETRY_TTL
            };
            if at.elapsed() < ttl {
                return info.clone();
            }
        }
    }
    let evidence = read_dialability_state();
    // The STUN rung needs the node's listen port. Probe the RPC once for it
    // (cheap, and the answer is cached right below with the rest).
    let listen_port = rpc_call(endpoint, "system_localListenAddresses", json!([]))
        .and_then(|v| v.as_array().cloned())
        .and_then(|a| a.iter().filter_map(|x| x.as_str()).find_map(port_of));
    let stun = stun_evidence(listen_port);
    let answer = read_p2p_info(endpoint, public_addr, evidence.as_deref(), stun.as_deref());
    if let Ok(mut guard) = p2p_cache().lock() {
        *guard = Some((Instant::now(), answer.clone()));
    }
    answer
}

fn models_cache() -> &'static std::sync::Mutex<Option<(Vec<String>, bool, std::time::Instant)>> {
    static C: std::sync::OnceLock<
        std::sync::Mutex<Option<(Vec<String>, bool, std::time::Instant)>>,
    > = std::sync::OnceLock::new();
    C.get_or_init(|| std::sync::Mutex::new(None))
}

/// Live model inventory from Ollama, cached for [`MODELS_TTL`]. Returns
/// (models, can_pull).
fn ollama_models(base: &str, fallback: &[String]) -> (Vec<String>, bool) {
    if let Ok(guard) = models_cache().lock() {
        if let Some((models, can_pull, at)) = guard.as_ref() {
            if gpn_agent::llm::infer::cache_is_fresh(*at, std::time::Instant::now(), MODELS_TTL) {
                return (models.clone(), *can_pull);
            }
        }
    }
    let answer = ollama_models_uncached(base, fallback);
    if let Ok(mut guard) = models_cache().lock() {
        *guard = Some((answer.0.clone(), answer.1, std::time::Instant::now()));
    }
    answer
}

fn ollama_models_uncached(base: &str, fallback: &[String]) -> (Vec<String>, bool) {
    let cli = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(2500))
        .build();
    match cli.and_then(|c| c.get(format!("{base}/api/tags")).send()) {
        Ok(r) => {
            let v: serde_json::Value = r.json().unwrap_or_default();
            let names: Vec<String> = v["models"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|m| m["name"].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if names.is_empty() {
                (fallback.to_vec(), true)
            } else {
                (names, true)
            }
        }
        Err(_) => (fallback.to_vec(), false),
    }
}

/// Total system RAM (MiB) from `/proc/meminfo`; 0 if unknown (e.g. Windows).
fn detect_ram_mb() -> u64 {
    if let Ok(s) = std::fs::read_to_string("/proc/meminfo") {
        for line in s.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                let kb: u64 = rest
                    .trim()
                    .trim_end_matches("kB")
                    .trim()
                    .parse()
                    .unwrap_or(0);
                return kb / 1024;
            }
        }
    }
    0
}

/// Drop models that cannot fit this box's VRAM, logging each skip **once** (the
/// access point polls `/status` every ~5 s; a per-poll warning would be noise).
fn vram_filter_logged(models: &[String], vram_mb: u64) -> Vec<String> {
    static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let mut out = Vec::with_capacity(models.len());
    for m in models {
        let fit = gpn_agent::models::check_model_vram(m, vram_mb);
        if let Some(reason) = fit.skip_reason(m) {
            let new = match SEEN.lock() {
                Ok(mut seen) => {
                    let new = !seen.contains(&reason);
                    if new {
                        seen.push(reason.clone());
                    }
                    new
                }
                Err(_) => false,
            };
            if new {
                eprintln!(
                    "[models] {reason} — not advertised (override: GPN_MODEL_VRAM_OVERRIDE={m})"
                );
            }
            continue;
        }
        out.push(m.clone());
    }
    out
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let listen = opt(&args, "--listen")
        .or_else(|| std::env::var("LISTEN").ok())
        .unwrap_or_else(|| "0.0.0.0:8093".into());
    let address = opt(&args, "--address")
        .or_else(|| std::env::var("ADDRESS").ok())
        .unwrap_or_default();
    let operator_gflops: u64 = opt(&args, "--gflops")
        .or_else(|| std::env::var("GFLOPS").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let models_csv = opt(&args, "--models")
        .or_else(|| std::env::var("MODELS").ok())
        .unwrap_or_else(|| "llama3.2:1b".into());
    let capacity: u64 = opt(&args, "--capacity")
        .or_else(|| std::env::var("CAPACITY").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let active: u64 = opt(&args, "--active")
        .or_else(|| std::env::var("ACTIVE").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let ollama = opt(&args, "--ollama")
        .or_else(|| std::env::var("GPN_OLLAMA").ok())
        .unwrap_or_else(|| "http://127.0.0.1:11434".into());
    // Peer-id publication: read the local node's libp2p identity over RPC so
    // the public directory can publish this provider as a dialable peer. The
    // node RPC defaults to the same box (`ws://127.0.0.1:9944`); a remote
    // endpoint can be given with `GPN_ENDPOINT` / `--endpoint`.
    let endpoint = opt(&args, "--endpoint")
        .or_else(|| std::env::var("GPN_ENDPOINT").ok())
        .unwrap_or_else(|| "ws://127.0.0.1:9944".into());
    // The address peers should dial — REQUIRED for a box behind NAT that
    // listens on 0.0.0.0. Accepts a multiaddr (`/ip4/1.2.3.4/tcp/30333`) or
    // `host:port`.
    let public_addr = opt(&args, "--public-addr").or_else(|| std::env::var("GPN_PUBLIC_ADDR").ok());
    // Job-type capabilities this node serves. A GPN node serves EVERY job type
    // by design ("one GPU, one job" = one job *at a time*, mutual exclusion —
    // NOT a limit on job types). Advertised so the scheduler can route
    // LLM/image/video/ML/fine-tune jobs here. Filtered to what this box
    // actually runs (a constrained box may opt a provider out).
    let caps_default = "LLMInference,ImageGeneration,VideoGeneration,MLInference,FineTuning";
    let capabilities_csv = opt(&args, "--capabilities")
        .or_else(|| std::env::var("CAPABILITIES").ok())
        .unwrap_or_else(|| caps_default.into());
    // Real compute economics for the rate card:
    //   --tok-per-s          the provider's MEASURED throughput (tokens/sec)
    //   --owner-cost-usd-hr  the provider's own cost/hr ($/kWh + amortized GPU)
    let tok_per_s: f64 = opt(&args, "--tok-per-s")
        .or_else(|| std::env::var("GPN_TOK_PER_S").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(40.0);
    let owner_cost_usd_hr: f64 = opt(&args, "--owner-cost-usd-hr")
        .or_else(|| std::env::var("GPN_OWNER_COST_USD_HR").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.03);
    // Memory bandwidth (GB/s) — the other half of the roofline. A hardware spec.
    let mem_bw_gbps: f64 = opt(&args, "--mem-bw-gbps")
        .or_else(|| std::env::var("GPN_MEM_BW_GBPS").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(250.0);
    // Hardware capability (item 14): advertise VRAM/RAM so a job's `min_vram_mb`/
    // `min_ram_mb` floor can be enforced *off-chain* by the scheduler. Auto-detected;
    // 0 = unknown (explicitly NOT filtered against, so pre-item-14 boxes keep working).
    let vram_mb: u64 = opt(&args, "--vram-mb")
        .or_else(|| std::env::var("VRAM_MB").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(detect_vram_mb);
    let ram_mb: u64 = opt(&args, "--ram-mb")
        .or_else(|| std::env::var("RAM_MB").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(detect_ram_mb);
    // Minimum GPU capability (item 19): advertise the GPU *class* + an FP32
    // TFLOPS estimate so the scheduler can gate a job's `min_tflops` floor and
    // reject CPU/APU integrated graphics. Auto-detected; overridable.
    let (gpu_class, tflops_detected) = detect_gpu_capability();
    let gpu_class: String = opt(&args, "--gpu-class")
        .or_else(|| std::env::var("GPU_CLASS").ok())
        .unwrap_or(gpu_class);
    let tflops: f64 = opt(&args, "--tflops")
        .or_else(|| std::env::var("TFLOPS").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(tflops_detected);
    let models: Vec<String> = models_csv
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let capabilities: Vec<String> = capabilities_csv
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let server = Server::http(&listen).map_err(|e| format!("cannot bind {listen}: {e}"))?;
    // #65: `gflops` is what the access point routes on, so advertise the CHAIN's
    // attested value (capped by `--gflops`) instead of a hand-typed number.
    let (gflops, gflops_source) = resolve_advertised_gflops(
        operator_gflops,
        if address.is_empty() {
            None
        } else {
            attested_gflops(&endpoint, &address)
        },
    );
    println!("GPN provider heartbeat on http://{listen} — address={address} gflops={gflops} ({gflops_source}) models={:?} active={active}/{capacity} capabilities={:?}", models, capabilities);
    if gflops == 0 {
        // Item 5: a 0 here reaches the marketplace as "no compute". Say so at
        // boot instead of letting a silent 0 look like a healthy heartbeat.
        eprintln!(
            "⚠ gflops=0: this heartbeat advertises NO compute throughput. The access point's \
             min_gflops filter will only route jobs that require none, and the on-chain \
             attestation tier is Bronze-0. Either let the provider attest (it measures and \
             submits on startup when the record is missing/stale) or measure it yourself \
             (`gpn_agent --attest` prints the measured FP32 GFLOPS) and pass it with \
             --gflops/GFLOPS, or set GPN_GFLOPS. If the measurement itself returns 0, the \
             OpenCL/CUDA device could not be opened — check the provider log for \
             'measure_gflops failed'."
        );
    }
    if !models.is_empty() {
        let usable = vram_filter_logged(&models, vram_mb);
        if usable.len() != models.len() {
            println!(
                "[models] advertising {}/{} configured model(s) after the VRAM gate (vram={vram_mb} MiB)",
                usable.len(),
                models.len()
            );
        }
    }

    // Item #43: warn LOUDLY at startup if the filesystem holding our data is
    // nearly full — a full disk silently halts the chain (2026-10-19 ENOSPC
    // block-persist failure). 0 = unknown, never treated as full.
    let disk_path = std::env::var("GPN_DATA_DIR")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".into());
    let boot_disk = disk_space(&disk_path);
    if let Some(pct) = boot_disk.free_percent() {
        if pct < LOW_DISK_PERCENT {
            eprintln!(
                "⚠ LOW DISK: {pct}% free on {disk_path} ({} MiB of {} MiB) — below {LOW_DISK_PERCENT}%; the chain will halt if this reaches 0 (see docs/run-a-node.md \"Disk headroom\")",
                boot_disk.free_mb, boot_disk.total_mb
            );
        }
    }

    let hdr = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
    for req in server.incoming_requests() {
        let (body, code) = match (req.method(), req.url()) {
            (Method::Get, "/status") => {
                let (models_now, can_pull) = ollama_models(&ollama, &models);
                // Item 3: never advertise a model this box's VRAM cannot hold —
                // the access point routes on `models`, so an unfittable entry is
                // a promise the box will OOM on.
                let models_now = vram_filter_logged(&models_now, vram_mb);
                // Item #43: report free space so a monitor can alert before
                // ENOSPC silently halts block production. 0/null = unknown.
                let disk = disk_space(
                    &std::env::var("GPN_DATA_DIR")
                        .or_else(|_| std::env::var("HOME"))
                        .unwrap_or_else(|_| ".".into()),
                );
                // #65: re-resolve per request (60 s cache) so a provider that
                // re-attests while this heartbeat runs is picked up without a
                // restart; the value never exceeds what the chain will accept.
                let (gflops_now, _) = resolve_advertised_gflops(
                    operator_gflops,
                    if address.is_empty() {
                        None
                    } else {
                        attested_gflops(&endpoint, &address)
                    },
                );
                let mut body = json!({
                    "address": address, "gflops": gflops_now, "models": models_now,
                    "can_pull": can_pull, "active": active, "capacity": capacity.max(1),
                    "capabilities": capabilities,
                    "tok_per_s": tok_per_s, "owner_cost_usd_hr": owner_cost_usd_hr,
                    "mem_bw_gbps": mem_bw_gbps, "vram_mb": vram_mb, "ram_mb": ram_mb,
                    "gpu_class": gpu_class, "tflops": tflops,
                    "disk_free_mb": disk.free_mb, "disk_total_mb": disk.total_mb,
                    "disk_free_pct": disk.free_percent()
                });
                // Peer-id publication (best-effort). Omitted — not null — when
                // the node RPC cannot be read, so consumers see the same shape
                // they always have.
                attach_p2p(
                    &mut body,
                    p2p_info(&endpoint, public_addr.as_deref()).as_ref(),
                );
                (body, 200)
            }
            (Method::Get, "/health") => (json!({"status":"ok"}), 200),
            // `POST /assign` was REMOVED in #46 Phase 3 step 2. The access point
            // never pushes work: delivery is the provider's `GET /assignments`
            // long-poll on the AP itself, so a provider behind NAT/CGNAT needs no
            // inbound port. Keeping a "one release" no-op would preserve the exact
            // inbound surface (and the shared secret) the pull transport exists to
            // remove, so an old AP's push gets a real 404 and the job is picked up
            // by the poll (which is written before any delivery attempt).
            _ => (json!({"error":"not found"}), 404),
        };
        let _ = req.respond(
            Response::from_string(body.to_string())
                .with_header(hdr.clone())
                .with_status_code(code),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    // The former `POST /assign` auth-gate tests were removed with the endpoint
    // itself (#46 Phase 3 step 2): the heartbeat no longer accepts assignments,
    // so there is nothing to authenticate — the pull endpoint on the AP is the
    // only delivery surface and it carries its own `GPN_ASSIGN_TOKEN` gate.

    use super::*;

    #[test]
    fn status_json_gets_the_fields_when_the_rpc_answers() {
        let pid = "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo";
        let rpc = |method: &str| -> Option<Value> {
            match method {
                "system_localPeerId" => Some(json!(pid)),
                "system_localListenAddresses" => Some(json!([
                    "/ip4/0.0.0.0/tcp/30333",
                    "/ip4/127.0.0.1/tcp/30333"
                ])),
                _ => None,
            }
        };
        let info = p2p_info_from_rpc(&rpc, Some("/ip4/203.0.113.7/tcp/30333"));
        assert!(info.is_some());
        let mut body = json!({"address": "5Abc", "gflops": 1});
        attach_p2p(&mut body, info.as_ref());
        assert_eq!(body["peer_id"], json!(pid));
        assert_eq!(body["p2p_port"], json!(30333));
        assert_eq!(
            body["multiaddrs"],
            json!([format!("/ip4/203.0.113.7/tcp/30333/p2p/{pid}")])
        );
        assert_eq!(body["listen_addrs"][0], json!("/ip4/0.0.0.0/tcp/30333"));
    }

    #[test]
    fn status_json_omits_the_fields_when_the_rpc_is_down() {
        let rpc = |_method: &str| -> Option<Value> { None };
        assert!(p2p_info_from_rpc(&rpc, None).is_none());
        let mut body = json!({"address": "5Abc"});
        attach_p2p(&mut body, None);
        assert!(body.get("peer_id").is_none());
        assert!(body.get("multiaddrs").is_none());
        assert!(body.get("p2p_port").is_none());
        // The heartbeat still has its normal fields.
        assert_eq!(body["address"], json!("5Abc"));
    }

    #[test]
    fn garbage_peer_id_from_rpc_publishes_nothing() {
        let rpc = |method: &str| -> Option<Value> {
            match method {
                "system_localPeerId" => Some(json!("../../etc/passwd")),
                "system_localListenAddresses" => Some(json!([])),
                _ => None,
            }
        };
        assert!(p2p_info_from_rpc(&rpc, Some("1.2.3.4:30333")).is_none());
    }

    #[test]
    fn peer_id_is_whitelisted() {
        let id = "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo";
        assert_eq!(normalize_peer_id(id).as_deref(), Some(id));
        assert_eq!(normalize_peer_id(&format!(" {id} ")).as_deref(), Some(id));
        assert_eq!(
            normalize_peer_id("QmYwAPJzv5CZsnA625s3Xf2nemtYgPpHdWEz79ojWnPbdG"),
            Some("QmYwAPJzv5CZsnA625s3Xf2nemtYgPpHdWEz79ojWnPbdG".into())
        );
        assert!(normalize_peer_id("").is_none());
        assert!(normalize_peer_id("12D3Koo/../../etc").is_none());
        assert!(normalize_peer_id("notapeeridbutlongenoughxxxxxxxxxxxxxxxxxxxxxxxx").is_none());
    }

    #[test]
    fn base_multiaddr_parses_the_operator_shapes() {
        assert_eq!(
            base_multiaddr("/ip4/1.2.3.4/tcp/30333").as_deref(),
            Some("/ip4/1.2.3.4/tcp/30333")
        );
        assert_eq!(
            base_multiaddr("1.2.3.4:30333").as_deref(),
            Some("/ip4/1.2.3.4/tcp/30333")
        );
        assert_eq!(
            base_multiaddr("https://Node.Example.org:30333/x").as_deref(),
            Some("/dns4/node.example.org/tcp/30333")
        );
        assert_eq!(
            base_multiaddr("[2001:db8::1]:30333").as_deref(),
            Some("/ip6/2001:db8::1/tcp/30333")
        );
        assert!(base_multiaddr("/ip4/1.2.3.4/tcp/30333/p2p/12D3KooWx").is_none());
        assert!(base_multiaddr("host-without-port").is_none());
        assert!(base_multiaddr("1.2.3.4:0").is_none());
        assert!(base_multiaddr("").is_none());
    }

    #[test]
    fn public_addr_wins_and_carries_the_peer_id() {
        let pid = "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo".to_string();
        let info = build_p2p_info(
            pid.clone(),
            Some("/ip4/203.0.113.7/tcp/30333"),
            vec!["/ip4/0.0.0.0/tcp/30333".into()],
        );
        assert_eq!(info.peer_id, pid);
        assert_eq!(
            info.multiaddrs,
            vec![format!("/ip4/203.0.113.7/tcp/30333/p2p/{pid}")]
        );
        assert_eq!(info.p2p_port, Some(30333));
        assert_eq!(
            info.listen_addrs,
            vec!["/ip4/0.0.0.0/tcp/30333".to_string()]
        );
    }

    #[test]
    fn a_wildcard_listener_publishes_the_port_but_no_fake_address() {
        let pid = "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo".to_string();
        let info = build_p2p_info(pid, None, vec!["/ip4/0.0.0.0/tcp/30333".into()]);
        // No public address is knowable — the AP combines p2p_port with the
        // host it already reached this provider on.
        assert!(info.multiaddrs.is_empty());
        assert_eq!(info.p2p_port, Some(30333));
    }

    #[test]
    fn private_and_loopback_listeners_are_not_published() {
        let pid = "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo".to_string();
        let info = build_p2p_info(
            pid.clone(),
            None,
            vec![
                "/ip4/127.0.0.1/tcp/30333".into(),
                "/ip4/192.168.1.5/tcp/30333".into(),
                "/ip4/100.64.1.2/tcp/30333".into(),
                "/ip6/::1/tcp/30333".into(),
                "/ip4/142.129.54.88/tcp/30334".into(),
            ],
        );
        assert_eq!(
            info.multiaddrs,
            vec![format!("/ip4/142.129.54.88/tcp/30334/p2p/{pid}")]
        );
        assert_eq!(info.p2p_port, Some(30334));
    }

    // ── Item 1: the emitter vs the access point's sanitizer ──────────────

    const PID: &str = "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo";

    /// THE regression test: live box2 emitted `listen_addrs` already carrying
    /// `/p2p/<id>` — doubled by an upstream litep2p bug — and the access point's
    /// sanitizer (`gpn_agent::seedlist::clean_published_multiaddr`, the same
    /// function the scheduler uses) rejected every one. Anything this emitter
    /// publishes must pass that gate.
    #[test]
    fn emitted_multiaddrs_round_trip_through_the_ap_sanitizer() {
        let info = build_p2p_info(
            PID.to_string(),
            Some("/ip4/142.129.54.88/tcp/30333"),
            vec![
                format!("/ip4/127.0.0.1/tcp/30333/p2p/{PID}/p2p/{PID}"),
                format!("/ip6/::1/tcp/30333/p2p/{PID}/p2p/{PID}"),
                format!("/ip4/192.168.1.20/tcp/30333/p2p/{PID}/p2p/{PID}"),
            ],
        );
        assert!(!info.multiaddrs.is_empty(), "must publish something");
        for ma in &info.multiaddrs {
            assert_eq!(
                gpn_agent::seedlist::clean_published_multiaddr(ma).as_deref(),
                Some(ma.as_str()),
                "the AP sanitizer must accept what we emit: {ma}"
            );
            // Exactly one /p2p/, and it is last.
            assert_eq!(ma.matches("/p2p/").count(), 1, "doubled suffix: {ma}");
            assert!(ma.ends_with(&format!("/p2p/{PID}")));
        }
        // listen_addrs are normalised too: no identity suffix at all.
        for ma in &info.listen_addrs {
            assert!(
                !ma.contains("/p2p/"),
                "listen_addrs must stay transport-only: {ma}"
            );
        }
        assert_eq!(
            info.listen_addrs,
            vec![
                "/ip4/127.0.0.1/tcp/30333".to_string(),
                "/ip6/::1/tcp/30333".to_string(),
                "/ip4/192.168.1.20/tcp/30333".to_string(),
            ]
        );
        assert_eq!(info.address_source, "GPN_PUBLIC_ADDR");
    }

    #[test]
    fn the_sanitizer_rejects_the_doubled_form_we_used_to_emit() {
        // Documents the failure mode: this is what box2 published and why the
        // access point dropped it. If the sanitizer ever accepted this, the
        // round-trip test above would stop meaning anything.
        let doubled = format!("/ip4/127.0.0.1/tcp/30333/p2p/{PID}/p2p/{PID}");
        assert!(gpn_agent::seedlist::clean_published_multiaddr(&doubled).is_none());
    }

    #[test]
    fn a_listener_with_one_p2p_suffix_is_collapsed_to_its_base() {
        let info = build_p2p_info_with(
            PID.to_string(),
            Some("/ip4/203.0.113.7/tcp/30333"),
            vec![format!("/ip4/0.0.0.0/tcp/30333/p2p/{PID}")],
            None,
            None,
        );
        assert_eq!(
            info.listen_addrs,
            vec!["/ip4/0.0.0.0/tcp/30333".to_string()]
        );
        assert_eq!(
            info.multiaddrs,
            vec![format!("/ip4/203.0.113.7/tcp/30333/p2p/{PID}")]
        );
    }

    // ── Item 2: never advertise a private address ───────────────────────

    #[test]
    fn every_private_range_is_filtered_from_the_ladder() {
        let private = [
            "/ip4/127.0.0.1/tcp/30333",
            "/ip4/10.1.2.3/tcp/30333",
            "/ip4/172.16.5.4/tcp/30333",
            "/ip4/172.31.255.254/tcp/30333",
            "/ip4/192.168.0.9/tcp/30333",
            "/ip4/100.64.0.1/tcp/30333",
            "/ip4/100.127.255.254/tcp/30333",
            "/ip4/169.254.1.1/tcp/30333",
            "/ip4/0.0.0.0/tcp/30333",
            "/ip6/::1/tcp/30333",
            "/ip6/fc00::1/tcp/30333",
            "/ip6/fe80::1/tcp/30333",
        ];
        let mut notes = Vec::new();
        let got = choose_public_base(None, &private.map(String::from), None, None, &mut notes);
        assert!(
            got.is_none(),
            "no private address may be published: {got:?}"
        );
        assert!(notes.iter().any(|n| n.contains("non-routable")));
        // And the emitted form keeps the peer id out of it either way.
        let info = build_p2p_info(PID.to_string(), None, private.map(String::from).to_vec());
        assert!(info.multiaddrs.is_empty());
        assert_eq!(info.address_source, "none");
    }

    #[test]
    fn a_public_listen_address_is_published_without_configuration() {
        let mut notes = Vec::new();
        let got = choose_public_base(
            None,
            &["/ip4/142.129.54.88/tcp/30333".to_string()],
            None,
            None,
            &mut notes,
        )
        .expect("a public listener is dialable");
        assert_eq!(got.base, "/ip4/142.129.54.88/tcp/30333");
        assert_eq!(got.source, AddrSource::PublicListen);
    }

    #[test]
    fn the_ladder_prefers_configured_then_node_evidence_then_stun() {
        let listen = ["/ip4/192.168.1.20/tcp/30333".to_string()];
        let mut notes = Vec::new();
        let configured = choose_public_base(
            Some("/ip4/203.0.113.7/tcp/30333"),
            &listen,
            Some("/ip4/198.51.100.9/tcp/30333"),
            Some("/ip4/198.51.100.10/tcp/30333"),
            &mut notes,
        )
        .unwrap();
        assert_eq!(configured.source, AddrSource::Configured);

        let mut notes = Vec::new();
        let evidence = choose_public_base(
            None,
            &listen,
            Some("/ip4/198.51.100.9/tcp/30333"),
            Some("/ip4/198.51.100.10/tcp/30333"),
            &mut notes,
        )
        .unwrap();
        assert_eq!(evidence.source, AddrSource::NodeEvidence);
        assert_eq!(evidence.base, "/ip4/198.51.100.9/tcp/30333");

        let mut notes = Vec::new();
        let stun = choose_public_base(
            None,
            &listen,
            None,
            Some("/ip4/198.51.100.10/tcp/30333"),
            &mut notes,
        )
        .unwrap();
        assert_eq!(stun.source, AddrSource::Stun);
        assert_eq!(stun.base, "/ip4/198.51.100.10/tcp/30333");
    }

    #[test]
    fn a_private_override_is_ignored_rather_than_published() {
        let mut notes = Vec::new();
        let got = choose_public_base(
            Some("127.0.0.1:30333"),
            &["/ip4/142.129.54.88/tcp/30333".to_string()],
            None,
            None,
            &mut notes,
        )
        .unwrap();
        // Falls through to the public listener; the bad override is reported.
        assert_eq!(got.source, AddrSource::PublicListen);
        assert!(notes.iter().any(|n| n.contains("not a public")));
    }

    #[test]
    fn nothing_dialable_publishes_an_empty_list_with_reasons() {
        let mut notes = Vec::new();
        let got = choose_public_base(
            None,
            &["/ip4/192.168.1.20/tcp/30333".to_string()],
            None,
            None,
            &mut notes,
        );
        assert!(got.is_none());
        assert!(
            notes.len() >= 3,
            "every rung must explain itself: {notes:?}"
        );
    }

    // ── Item 3: VRAM gating of the advertised inventory ──────────────────

    #[test]
    fn the_status_inventory_drops_models_that_cannot_fit() {
        let models: Vec<String> = ["llama3.2:1b", "qwen2.5:7b", "deepseek-r1:14b"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let kept = vram_filter_logged(&models, 8188);
        assert_eq!(
            kept,
            vec!["llama3.2:1b".to_string(), "qwen2.5:7b".to_string()]
        );
        // Unknown VRAM (0) filters nothing — never block on ignorance.
        assert_eq!(vram_filter_logged(&models, 0).len(), 3);
    }

    // ── Queue #65: advertise what the chain will actually accept ──────────

    /// A real `AttestationRecord` read off the live chain for the local
    /// provider (30 000 GFLOPS, Bronze) — the exact bytes `/status` must decode.
    fn live_record(gflops: u64) -> Vec<u8> {
        let mut b = vec![0u8; 82];
        b[0] = 0x33; // submitted_at = 51
        b[68] = 1; // gpu_count
        b[69..77].copy_from_slice(&gflops.to_le_bytes());
        b[77] = 1; // tier = Bronze
        b
    }

    #[test]
    fn the_attestation_record_decodes_to_its_gflops() {
        assert_eq!(decode_attested_gflops(&live_record(30_000)), Some(30_000));
        assert_eq!(decode_attested_gflops(&live_record(188)), Some(188));
        assert_eq!(decode_attested_gflops(&live_record(0)), Some(0));
        // A short/garbage read is "unknown", never a fabricated 0.
        assert_eq!(decode_attested_gflops(&[]), None);
        assert_eq!(decode_attested_gflops(&[0u8; 76]), None);
    }

    #[test]
    fn the_storage_key_is_the_attestations_map_entry_for_the_account() {
        // ss58 and 0x-hex are the same account → the same key; a different
        // account gets a different one (otherwise every box would advertise
        // the first provider's attestation).
        let a = account_pubkey("5ENqSp5Q65bxfgryZExQDRnV1rEW1qb2UBztvswLamkoUrqj").unwrap();
        let b = account_pubkey(
            "0x664c6e12d0eff26c646b764f733f0468d44deea02f856ab9f1b629af06993d01",
        )
        .unwrap();
        assert_eq!(a, b);
        assert_eq!(attestation_key(&a), attestation_key(&b));
        let c = account_pubkey("5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty").unwrap();
        assert_ne!(attestation_key(&a), attestation_key(&c));
        assert!(attestation_key(&a).starts_with("0x"));
        assert_eq!(attestation_key(&a).len(), 2 + 80 * 2); // twox128 + twox128 + blake2_128_concat
        assert!(account_pubkey("not-an-address").is_none());
    }

    #[test]
    fn advertised_gflops_never_exceeds_what_the_chain_accepts() {
        // box1's actual shape: the operator never passed a number.
        assert_eq!(resolve_advertised_gflops(0, Some(188)).0, 188);
        // Operator over-advertised (2500 typed, 188 attested): the chain gate
        // would reject the job at accept_job, so advertise the chain's number.
        assert_eq!(resolve_advertised_gflops(2500, Some(188)).0, 188);
        // Operator deliberately advertises less than attested: keep it.
        assert_eq!(resolve_advertised_gflops(100, Some(30_000)).0, 100);
        // Both real and equal: unchanged.
        assert_eq!(resolve_advertised_gflops(30_000, Some(30_000)).0, 30_000);
        // Chain unreadable / never attested: fall back to the operator value
        // (never silently to 0 when the operator knows a number).
        assert_eq!(resolve_advertised_gflops(2500, None).0, 2500);
        assert_eq!(resolve_advertised_gflops(2500, Some(0)).0, 2500);
        // Nothing known anywhere: 0, and the boot warning fires.
        assert_eq!(resolve_advertised_gflops(0, None).0, 0);
        assert_eq!(resolve_advertised_gflops(0, Some(0)).0, 0);
    }
}
