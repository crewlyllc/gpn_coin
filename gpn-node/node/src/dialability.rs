//! Dialability gate — **every GPN node must be dialable to join the network**.
//!
//! Policy (Jeremy): a robust GPN mesh needs *every* node to be inbound
//! reachable — a working public address **and** a forwarded port. A node that
//! is only outbound-reachable (behind NAT with no port forward) can dial out
//! and sync, but no peer can ever dial *it*; that turns the "mesh" into a star
//! around the bootnode and makes the node useless as a peer for everyone else.
//! Such a node must therefore **not join / participate**. It has to fail
//! loudly and exit, not silently limp along.
//!
//! # How dialability is established
//!
//! After the network worker starts we look for **direct evidence that someone
//! can reach us** (not merely that we can reach them):
//!
//! 1. **An inbound connection** — a peer dialed *us*. In `NetworkState` this
//!    is a connected peer whose endpoint is [`PeerEndpoint::Listening`]
//!    (libp2p `ConnectedPoint::Listener`). This is the strongest signal.
//! 2. **A configured public address confirmed by observation** — if
//!    `GPN_PUBLIC_ADDR` (or `--public-addr`) is `/ip4/1.2.3.4/tcp/30333`, then
//!    an observed external address `/ip4/1.2.3.4/tcp/30333/...` from peer
//!    Identify means our listener was reached *through the forward* at exactly
//!    that address:port.
//! 3. **A public listen address** — the node is bound directly to a public IP
//!    (the classic VPS case), so peers can dial the listening address itself.
//! 4. **Observed public address on the listen port** (only when no public
//!    address was configured) — Identify told us we are reachable at
//!    `<public-ip>:<same-port-we-listen-on>`, i.e. a port-preserving mapping.
//!
//! Anything else — only loopback/private/listen-on-0.0.0.0 with no observation
//! — is NOT dialable and fails the gate after the grace window.
//!
//! # Evidence model (v2.3)
//!
//! The gate passes on **any one** of these, in this order of strength:
//!
//! | evidence | meaning |
//! |---|---|
//! | `inbound_peer` | a peer opened a connection to us |
//! | `configured_addr_matches_observation` | the configured `GPN_PUBLIC_ADDR` endpoint (IP literal, or an IP a `/dns4`/`/dns6` name resolves to) was reported by Identify as our external address |
//! | `observed_public_addr` | no configured address, but peers see us at a public `ip:port` on our listen port |
//! | `stun_mapped_addr` | an **external** STUN binding tells us the NAT maps our socket to exactly the configured endpoint |
//! | `tcp_probe_ok` | a bounded outbound TCP connect to the configured public `ip:port` succeeded (hairpin; weakest — a router may hairpin even when the WAN forward is closed) |
//!
//! `stun_mapped_addr` and `tcp_probe_ok` exist because the incident of
//! 2026-10-20 showed the *observation-only* model produces false negatives:
//! the Linux bootnode was externally reachable (3/3 independent probes) yet no
//! libp2p peer completed Identify against it inside the grace window, so the
//! sole authority exited and the chain halted for ~12 minutes.
//!
//! # `/dns4` is a first-class public address
//!
//! `GPN_PUBLIC_ADDR=/dns4/gpn-testnet.duckdns.org/tcp/30333` is accepted: the
//! name is **resolved at check time** and *every* resolved IP must classify as
//! public. Matching against peer observations is done on the **resolved IPs**,
//! never on the literal hostname (peers only ever report IPs).
//!
//! # The gate may warn but must never halt the chain
//!
//! [`safety_valve`] is consulted before a hard-mode exit. On a solo chain, a
//! dev/`--alice` style chain, or a network where this node is the only
//! authority visible (0–1 connected peers), the gate **downgrades itself to
//! soft mode** with a prominent warning instead of exiting: a false negative
//! costs an operator a confusing restart, but exiting the last authoring node
//! costs the network its chain.
//!
//! # Behaviour (v2.2 — dialable is REQUIRED by default)
//!
//! A node is a **backbone node**: it announces its endpoint and accepts inbound
//! peers. The network is still growing, so **inbound reachability is a hard
//! requirement by default** — a node that cannot be dialled prints the full
//! operator notice and exits with [`EXIT_NOT_DIALABLE`] (78) after the grace
//! window.
//!
//! * Probe is ON by default. `GPN_REQUIRE_DIALABLE=0` (also `false`/`no`/`off`)
//!   skips the probe entirely (local/single-node testing).
//! * Hard gate is the **default**: `--require-backbone` is implicit;
//!   `GPN_REQUIRE_BACKBONE` defaults to ON.
//! * Opt out with `--no-require-backbone` / `GPN_REQUIRE_BACKBONE=0`: the node
//!   logs the actionable [`degradation_notice`] block once and **continues** as
//!   a non-discoverable participant, re-checking every 10 minutes and printing
//!   [`promotion_notice`] once when it becomes dialable.
//! * `GPN_SEED=0` / `--no-seed` silences the notice (the operator asked to be
//!   private). `GPN_DISCOVERABLE=0` / `--no-discoverable` implies `--no-seed`.
//! * Grace window: `GPN_DIALABLE_TIMEOUT_SECS` (default 120 s).
//!
//! The pure decision functions ([`classify_ip`], [`evaluate`],
//! [`degradation_notice`]) are unit-tested without a network;
//! [`wait_for_dialability`] is exercised with a stub.

use sc_network::multiaddr::{Multiaddr, Protocol};
use sc_network::network_state::{NetworkState, PeerEndpoint};
use sc_network::{NetworkStateInfo, NetworkStatusProvider};
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// Distinct process exit code used when the node is not dialable.
///
/// 78 is `EX_CONFIG` (sysexits.h): "configuration error". The `gpn_node`
/// supervisor maps it to `down(not dialable)` in the status line instead of a
/// generic crash.
pub const EXIT_NOT_DIALABLE: i32 = 78;

/// Default grace window for evidence of inbound reachability.
pub const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Default inbound port (`GPN_SEED_PORT`); also the recommended external port.
pub const DEFAULT_SEED_PORT: u16 = 30333;

/// Example LAN IP used in the "recommended setup" block of the operator
/// notice. It is deliberately a *documentation* example — the detected LAN IP
/// is printed separately, right next to it.
pub const RECOMMENDED_LAN_EXAMPLE: &str = "192.168.1.50";

/// How often a running node re-checks its inbound reachability (design §5.3).
pub const RECHECK_SECS: u64 = 600;

/// How often a running node re-checks its own public address.
pub const PUBLIC_IP_RECHECK_SECS: u64 = 300;

/// Bound on the outbound TCP reachability probe (hairpin test).
pub const TCP_PROBE_TIMEOUT_SECS: u64 = 5;

/// Bound on the external STUN binding probe.
pub const STUN_TIMEOUT_SECS: u64 = 3;

/// Default STUN server (any RFC 5389 server works).
pub const DEFAULT_STUN_SERVER: &str = "stun.l.google.com:19302";

/// Default bound on the public-IP echo request.
pub const PUBLIC_IP_TIMEOUT_SECS: u64 = 5;

/// Re-resolve a `/dns4`/`/dns6` public address at most this often.
pub const DNS_REFRESH_SECS: u64 = 30;

/// Re-run the active (STUN/TCP) probes at most this often while waiting.
const PROBE_REFRESH_SECS: u64 = 15;

/// Poll cadence while waiting for the gate.
const POLL_SECS: u64 = 5;
/// Log a progress line this often while waiting.
const PROGRESS_EVERY_SECS: u64 = 15;

/// Classification of an IP address for dialability purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpClass {
    /// Globally routable — a peer on the internet can dial it.
    Public,
    /// RFC1918 / IPv6 ULA / CGNAT — only reachable inside a private network.
    Private,
    /// `127.0.0.0/8`, `::1`.
    Loopback,
    /// `169.254.0.0/16`, `fe80::/10`.
    LinkLocal,
    /// `0.0.0.0`, `::`.
    Unspecified,
    /// Multicast / broadcast — never a node address.
    Multicast,
}

impl IpClass {
    /// Only a globally routable address can be dialed from the internet.
    pub fn is_public(self) -> bool {
        matches!(self, IpClass::Public)
    }
}

/// Classify an IP address (works for both v4 and v6).
pub fn classify_ip(ip: &IpAddr) -> IpClass {
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_unspecified() {
                IpClass::Unspecified
            } else if v4.is_loopback() {
                IpClass::Loopback
            } else if v4.is_link_local() {
                IpClass::LinkLocal
            } else if v4.is_private()
                || v4.is_broadcast()
                || v4.is_documentation()
                // 100.64.0.0/10 — carrier-grade NAT. Not reachable from the
                // public internet even though it is not RFC1918.
                || (v4.octets()[0] == 100 && (64..=127).contains(&v4.octets()[1]))
            {
                IpClass::Private
            } else if v4.is_multicast() {
                IpClass::Multicast
            } else {
                IpClass::Public
            }
        }
        IpAddr::V6(v6) => {
            if v6.is_unspecified() {
                IpClass::Unspecified
            } else if v6.is_loopback() {
                IpClass::Loopback
            } else if v6.is_unicast_link_local() {
                IpClass::LinkLocal
            } else if v6.is_unique_local() {
                IpClass::Private
            } else if v6.is_multicast() {
                IpClass::Multicast
            } else {
                IpClass::Public
            }
        }
    }
}

/// The host half of a multiaddress endpoint: a literal IP or a DNS name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Host {
    Ip(IpAddr),
    Dns(String),
}

impl std::fmt::Display for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Host::Ip(ip) => write!(f, "{ip}"),
            Host::Dns(name) => f.write_str(name),
        }
    }
}

/// Extract `(host, port)` from a multiaddress, accepting `/ip4`, `/ip6`,
/// `/dns`, `/dns4` and `/dns6` hosts. Trailing `/p2p/…` (and anything else)
/// is ignored. Returns `None` unless both a host and a non-zero TCP/UDP port
/// are present.
pub fn host_port_of(ma: &Multiaddr) -> Option<(Host, u16)> {
    let mut host = None;
    let mut port = None;
    for proto in ma.iter() {
        match proto {
            Protocol::Ip4(v4) => host = Some(Host::Ip(IpAddr::V4(v4))),
            Protocol::Ip6(v6) => host = Some(Host::Ip(IpAddr::V6(v6))),
            Protocol::Dns(n) | Protocol::Dns4(n) | Protocol::Dns6(n) => {
                host = Some(Host::Dns(n.to_string()))
            }
            Protocol::Tcp(p) | Protocol::Udp(p) => port = Some(p),
            _ => {}
        }
    }
    match (host, port) {
        (Some(h), Some(p)) if p != 0 => Some((h, p)),
        _ => None,
    }
}

/// Resolve a DNS name to every address the system resolver returns.
///
/// Blocking (`getaddrinfo`); call it from `spawn_blocking` in async code. An
/// empty vector means "could not resolve" and is treated as *no evidence*
/// rather than as a hard failure by itself.
pub fn resolve_host(name: &str, port: u16) -> Vec<IpAddr> {
    use std::net::ToSocketAddrs;
    let mut out: Vec<IpAddr> = match (name, port).to_socket_addrs() {
        Ok(it) => it.map(|sa| sa.ip()).collect(),
        Err(_) => Vec::new(),
    };
    out.sort();
    out.dedup();
    out
}

/// Extract `(ip, port)` from a multiaddress, ignoring `/p2p/…` and any other
/// trailing components. Returns `None` unless both an IP and a non-zero
/// TCP/UDP port are present. DNS hosts return `None` — use [`host_port_of`]
/// when `/dns4`/`/dns6` must be accepted.
pub fn endpoint_of(ma: &Multiaddr) -> Option<(IpAddr, u16)> {
    let mut ip = None;
    let mut port = None;
    for proto in ma.iter() {
        match proto {
            Protocol::Ip4(v4) => ip = Some(IpAddr::V4(v4)),
            Protocol::Ip6(v6) => ip = Some(IpAddr::V6(v6)),
            Protocol::Tcp(p) | Protocol::Udp(p) => port = Some(p),
            _ => {}
        }
    }
    match (ip, port) {
        (Some(ip), Some(port)) if port != 0 => Some((ip, port)),
        _ => None,
    }
}

/// Why the gate decided the node *is* dialable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Evidence {
    /// A peer opened an inbound connection to us (`Endpoint::Listener`).
    InboundPeer,
    /// The configured public address was observed on the wire (Identify).
    ConfiguredAddrObserved,
    /// We are listening directly on a public IP.
    PublicListenAddr,
    /// No configured address, but our observed public address uses the port we
    /// listen on (port-preserving mapping / forward).
    ObservedPublicAtListenPort,
    /// An external STUN binding reported a mapped endpoint equal to the
    /// configured public address (the NAT really maps us there).
    StunMappedAddr,
    /// A bounded outbound TCP connect to the configured public endpoint worked
    /// (hairpin). Weakest evidence: some routers hairpin a closed WAN port.
    TcpProbeOk,
}

impl std::fmt::Display for Evidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Evidence::InboundPeer => "inbound peer connection",
            Evidence::ConfiguredAddrObserved => "configured public address observed",
            Evidence::PublicListenAddr => "public listen address",
            Evidence::ObservedPublicAtListenPort => "observed public address on listen port",
            Evidence::StunMappedAddr => "external STUN mapping matches the configured address",
            Evidence::TcpProbeOk => "outbound TCP reachability probe succeeded",
        };
        f.write_str(s)
    }
}

/// The gate's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// `true` iff the node is inbound-reachable.
    pub dialable: bool,
    /// The evidence that satisfied the gate, if any.
    pub evidence: Option<Evidence>,
    /// Human-readable explanation (always set) for logs / diagnostics.
    pub detail: String,
}

impl Verdict {
    fn pass(evidence: Evidence, detail: impl Into<String>) -> Self {
        Verdict {
            dialable: true,
            evidence: Some(evidence),
            detail: detail.into(),
        }
    }

    fn fail(detail: impl Into<String>) -> Self {
        Verdict {
            dialable: false,
            evidence: None,
            detail: detail.into(),
        }
    }
}

/// Count connected peers that **dialed us** (inbound / listener endpoint).
pub fn inbound_peer_count(state: &NetworkState) -> usize {
    state
        .connected_peers
        .values()
        .filter(|p| matches!(p.endpoint, PeerEndpoint::Listening { .. }))
        .count()
}

/// Everything the pure gate decision needs.
///
/// `resolved` holds the IPs the configured `/dns4`/`/dns6` name resolved to at
/// check time (empty for an IP literal, and for a name that failed to resolve).
/// `stun_mapped` / `tcp_probe_ok` are the *active* probes from
/// [`probe_configured_endpoint`]; they are only meaningful when a public
/// address was configured.
#[derive(Debug, Clone)]
pub struct GateInputs<'a> {
    /// Peers that dialed us (see [`inbound_peer_count`]).
    pub inbound_peers: usize,
    /// Our listen addresses (`NetworkStateInfo::listen_addresses`).
    pub listen: &'a [Multiaddr],
    /// Addresses peers reported for us (`external_addresses`).
    pub observed: &'a [Multiaddr],
    /// `GPN_PUBLIC_ADDR` / `--public-addr`, if any.
    pub configured: Option<&'a Multiaddr>,
    /// Resolved IPs of a configured DNS name (empty when not a name).
    pub resolved: &'a [IpAddr],
    /// The external STUN binding reported exactly our configured endpoint.
    pub stun_mapped: bool,
    /// A bounded outbound TCP connect to the configured endpoint succeeded.
    pub tcp_probe_ok: bool,
}

impl<'a> GateInputs<'a> {
    /// Minimal inputs (no DNS resolution, no active probes).
    pub fn new(
        inbound_peers: usize,
        listen: &'a [Multiaddr],
        observed: &'a [Multiaddr],
        configured: Option<&'a Multiaddr>,
    ) -> Self {
        GateInputs {
            inbound_peers,
            listen,
            observed,
            configured,
            resolved: &[],
            stun_mapped: false,
            tcp_probe_ok: false,
        }
    }
}

/// The pure gate decision.
///
/// * `inbound_peers` — peers that dialed us (see [`inbound_peer_count`]).
/// * `listen` — the node's listen addresses (`NetworkStateInfo::listen_addresses`).
/// * `observed` — addresses peers reported for us (`external_addresses`).
/// * `configured` — `GPN_PUBLIC_ADDR` / `--public-addr`, if any.
///
/// This is the passive-only form; see [`evaluate_with`] for DNS resolution and
/// the active probes.
pub fn evaluate(
    inbound_peers: usize,
    listen: &[Multiaddr],
    observed: &[Multiaddr],
    configured: Option<&Multiaddr>,
) -> Verdict {
    evaluate_with(&GateInputs::new(inbound_peers, listen, observed, configured))
}

/// The pure gate decision, including DNS resolution and active probes.
pub fn evaluate_with(inputs: &GateInputs) -> Verdict {
    let GateInputs {
        inbound_peers,
        listen,
        observed,
        configured,
        resolved,
        stun_mapped,
        tcp_probe_ok,
    } = inputs;
    let (inbound_peers, listen, observed, resolved, stun_mapped, tcp_probe_ok) = (
        *inbound_peers,
        *listen,
        *observed,
        *resolved,
        *stun_mapped,
        *tcp_probe_ok,
    );
    let configured = *configured;

    if inbound_peers > 0 {
        return Verdict::pass(
            Evidence::InboundPeer,
            format!("{inbound_peers} peer(s) dialed this node (inbound connection)"),
        );
    }

    // A configured address is authoritative: require a match on the endpoint it
    // names — the literal IP, or (for /dns4, /dns6) any IP it resolves to.
    if let Some(cfg_ma) = configured {
        let (host, port) = match host_port_of(cfg_ma) {
            Some(hp) => hp,
            None => {
                return Verdict::fail(format!(
                    "GPN_PUBLIC_ADDR {cfg_ma} is not a usable \
                     /ip4|ip6|dns4|dns6/<host>/tcp/<port> multiaddress"
                ));
            }
        };

        // The endpoints that would prove this address: (ip, port) pairs.
        let endpoints: Vec<(IpAddr, u16)> = match &host {
            Host::Ip(ip) => vec![(*ip, port)],
            Host::Dns(name) => {
                if resolved.is_empty() {
                    return Verdict::fail(format!(
                        "GPN_PUBLIC_ADDR {cfg_ma} uses DNS name '{name}', which did not \
                         resolve to any address at check time"
                    ));
                }
                resolved.iter().map(|ip| (*ip, port)).collect()
            }
        };

        // Every resolved IP must be globally routable: an address that resolves
        // to a private/loopback IP cannot be dialled from the internet, and a
        // mixed answer is a misconfiguration we must not paper over.
        if let Some((bad_ip, class)) = endpoints
            .iter()
            .map(|(ip, _)| (*ip, classify_ip(ip)))
            .find(|(_, c)| !c.is_public())
        {
            return Verdict::fail(format!(
                "GPN_PUBLIC_ADDR {cfg_ma} resolves to NON-public IP {bad_ip} ({}); a public \
                 (globally routable) address is required",
                class_desc(class)
            ));
        }

        for o in observed {
            if let Some((ip, oport)) = endpoint_of(o) {
                if oport == port && endpoints.iter().any(|(eip, _)| *eip == ip) {
                    let via = match &host {
                        Host::Dns(name) => format!(" ({name} resolves to {ip})"),
                        Host::Ip(_) => String::new(),
                    };
                    return Verdict::pass(
                        Evidence::ConfiguredAddrObserved,
                        format!(
                            "observed external address matches GPN_PUBLIC_ADDR ({cfg_ma}){via}"
                        ),
                    );
                }
            }
        }
        for l in listen {
            if let Some((ip, lport)) = endpoint_of(l) {
                if lport == port && endpoints.iter().any(|(eip, _)| *eip == ip) {
                    return Verdict::pass(
                        Evidence::PublicListenAddr,
                        format!("listening directly on the public address {cfg_ma}"),
                    );
                }
            }
        }

        // Active evidence. The external STUN mapping is much stronger than a
        // hairpin connect, so it is reported first.
        if stun_mapped {
            return Verdict::pass(
                Evidence::StunMappedAddr,
                format!(
                    "external STUN binding reports our mapped endpoint as {cfg_ma} \
                     (NAT maps the public port to this node)"
                ),
            );
        }
        if tcp_probe_ok {
            return Verdict::pass(
                Evidence::TcpProbeOk,
                format!(
                    "outbound TCP connect to {cfg_ma} succeeded (hairpin reachability; \
                     weaker than peer observation)"
                ),
            );
        }

        return Verdict::fail(format!(
            "configured public address {cfg_ma} was neither observed by peers nor bound \
             locally, and neither the STUN probe nor the TCP probe could confirm it — the \
             port is probably not forwarded (resolved: [{}], observed: [{}], listening: [{}])",
            join_ips(resolved),
            join(observed),
            join(listen)
        ));
    }

    // No configured address: accept a direct public bind (VPS), or an observed
    // public endpoint on the port we listen on.
    for l in listen {
        if let Some((ip, port)) = endpoint_of(l) {
            if classify_ip(&ip).is_public() {
                return Verdict::pass(
                    Evidence::PublicListenAddr,
                    format!("listening on public address /{}/{port}", ip),
                );
            }
        }
    }

    let listen_ports: Vec<u16> = listen
        .iter()
        .filter_map(|l| endpoint_of(l).map(|(_, p)| p))
        .collect();
    for o in observed {
        if let Some((ip, port)) = endpoint_of(o) {
            if classify_ip(&ip).is_public() && listen_ports.contains(&port) {
                return Verdict::pass(
                    Evidence::ObservedPublicAtListenPort,
                    format!("observed public endpoint /{}/{port} on the listen port", ip),
                );
            }
        }
    }

    Verdict::fail(format!(
        "no inbound peer, no public listen address and no matching observed address \
         (observed: [{}], listening: [{}])",
        join(observed),
        join(listen)
    ))
}

fn join(addrs: &[Multiaddr]) -> String {
    if addrs.is_empty() {
        return "none".into();
    }
    addrs
        .iter()
        .map(|a| a.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn join_ips(ips: &[IpAddr]) -> String {
    if ips.is_empty() {
        return "none".into();
    }
    ips.iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn class_desc(c: IpClass) -> &'static str {
    match c {
        IpClass::Public => "public",
        IpClass::Private => "private/RFC1918",
        IpClass::Loopback => "loopback",
        IpClass::LinkLocal => "link-local",
        IpClass::Unspecified => "unspecified",
        IpClass::Multicast => "multicast",
    }
}

// ── Active evidence: external STUN binding + bounded TCP probe ──────────

/// Build an RFC 5389 Binding Request (20 bytes, no attributes).
///
/// Message type `0x0001`, length 0, magic cookie `0x2112A442`, and a
/// time/pid-derived 96-bit transaction id. Pure, so the wire format is
/// unit-testable.
pub fn build_stun_binding_request() -> [u8; 20] {
    let mut msg = [0u8; 20];
    msg[0..2].copy_from_slice(&0x0001u16.to_be_bytes());
    msg[2..4].copy_from_slice(&0u16.to_be_bytes());
    msg[4..8].copy_from_slice(&[0x21, 0x12, 0xA4, 0x42]);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    msg[8..16].copy_from_slice(&now.to_be_bytes());
    msg[16..20].copy_from_slice(&(std::process::id() as u32).to_be_bytes());
    msg
}

/// Parse `XOR-MAPPED-ADDRESS` (0x0020, preferred) or the deprecated
/// `MAPPED-ADDRESS` (0x0001) from a STUN Binding Success Response.
///
/// Returns `None` for anything that is not a well-formed success response, so a
/// spoofed or garbled packet can never be mistaken for evidence.
pub fn parse_stun_xor_mapped(msg: &[u8]) -> Option<(IpAddr, u16)> {
    if msg.len() < 20 {
        return None;
    }
    // Binding Success Response = 0x0101.
    if u16::from_be_bytes([msg[0], msg[1]]) != 0x0101 {
        return None;
    }
    if msg[4..8] != [0x21, 0x12, 0xA4, 0x42] {
        return None;
    }
    let tid: [u8; 12] = msg[8..20].try_into().ok()?;
    let attr_len = u16::from_be_bytes([msg[2], msg[3]]) as usize;
    let body_end = (20 + attr_len).min(msg.len());
    let body = &msg[20..body_end];

    let mut at = 0usize;
    let mut fallback: Option<(IpAddr, u16)> = None;
    while at + 4 <= body.len() {
        let kind = u16::from_be_bytes([body[at], body[at + 1]]);
        let len = u16::from_be_bytes([body[at + 2], body[at + 3]]) as usize;
        let val_start = at + 4;
        let Some(val_end) = val_start.checked_add(len) else {
            break;
        };
        if val_end > body.len() {
            break;
        }
        let val = &body[val_start..val_end];
        match kind {
            0x0020 => {
                if let Some(ep) = decode_stun_mapped(val, true, &tid) {
                    return Some(ep);
                }
            }
            0x0001 if fallback.is_none() => {
                fallback = decode_stun_mapped(val, false, &tid);
            }
            _ => {}
        }
        // Attributes are padded to a 4-byte boundary.
        at = val_start + ((len + 3) & !3);
    }
    fallback
}

fn decode_stun_mapped(val: &[u8], xor: bool, tid: &[u8; 12]) -> Option<(IpAddr, u16)> {
    if val.len() < 4 {
        return None;
    }
    let family = val[1];
    let raw_port = u16::from_be_bytes([val[2], val[3]]);
    let port = if xor { raw_port ^ 0x2112 } else { raw_port };
    match family {
        0x01 if val.len() >= 8 => {
            let mut o = [val[4], val[5], val[6], val[7]];
            if xor {
                let cookie = [0x21u8, 0x12, 0xA4, 0x42];
                for i in 0..4 {
                    o[i] ^= cookie[i];
                }
            }
            Some((IpAddr::V4(std::net::Ipv4Addr::from(o)), port))
        }
        0x02 if val.len() >= 20 => {
            let mut o = [0u8; 16];
            o.copy_from_slice(&val[4..20]);
            if xor {
                // IPv6 XORs with cookie || transaction id.
                let mut mask = [0u8; 16];
                mask[0..4].copy_from_slice(&[0x21, 0x12, 0xA4, 0x42]);
                mask[4..16].copy_from_slice(tid);
                for i in 0..16 {
                    o[i] ^= mask[i];
                }
            }
            Some((IpAddr::V6(std::net::Ipv6Addr::from(o)), port))
        }
        _ => None,
    }
}

/// Bounded outbound TCP connect: "can this host reach its own public endpoint".
///
/// From the node itself this is a **hairpin** test. It can fail on a router
/// without NAT loopback even when the WAN forward is open, so a failure is
/// never proof of non-dialability — only a success is evidence.
pub fn tcp_probe(ip: IpAddr, port: u16, timeout: Duration) -> bool {
    use std::net::{SocketAddr, TcpStream};
    TcpStream::connect_timeout(&SocketAddr::new(ip, port), timeout).is_ok()
}

/// External STUN binding: what endpoint does the NAT map our socket to?
///
/// Genuinely external evidence (a third party sees our mapped tuple), unlike a
/// hairpin connect. Returns `None` on any error/timeout.
pub async fn stun_probe(server: &str, timeout: Duration) -> Option<(IpAddr, u16)> {
    let sock = tokio::net::UdpSocket::bind("0.0.0.0:0").await.ok()?;
    sock.connect(server).await.ok()?;
    sock.send(&build_stun_binding_request()).await.ok()?;
    let mut buf = [0u8; 512];
    let n = tokio::time::timeout(timeout, sock.recv(&mut buf))
        .await
        .ok()?
        .ok()?;
    parse_stun_xor_mapped(&buf[..n])
}

/// Gather active evidence for a configured public address.
///
/// Order of preference: external STUN first (strongest, and it proves the
/// *forward*, not just hairpin); only when that does not already name our
/// configured endpoint do we spend [`TCP_PROBE_TIMEOUT_SECS`] on the hairpin
/// connect. A DNS name is resolved once per call.
///
/// Returns `(resolved_ips, stun_matched, tcp_probe_ok)`.
pub async fn probe_configured_endpoint(
    configured: &Multiaddr,
    stun_server: Option<&str>,
) -> (Vec<IpAddr>, bool, bool) {
    let Some((host, port)) = host_port_of(configured) else {
        return (Vec::new(), false, false);
    };

    let resolved = match &host {
        Host::Ip(ip) => vec![*ip],
        Host::Dns(name) => {
            let name = name.clone();
            tokio::task::spawn_blocking(move || resolve_host(&name, port))
                .await
                .unwrap_or_default()
        }
    };
    if resolved.is_empty() {
        return (resolved, false, false);
    }

    if let Some(server) = stun_server {
        if let Some((ip, mapped_port)) =
            stun_probe(server, Duration::from_secs(STUN_TIMEOUT_SECS)).await
        {
            if mapped_port == port && resolved.iter().any(|r| *r == ip) {
                return (resolved, true, false);
            }
            log::debug!(
                "dialability: STUN reports {ip}:{mapped_port}, which is not the configured \
                 {configured} — trying the TCP probe"
            );
        }
    }

    let probe_ips = resolved.clone();
    let ok = tokio::task::spawn_blocking(move || {
        let timeout = Duration::from_secs(TCP_PROBE_TIMEOUT_SECS);
        probe_ips.iter().any(|ip| tcp_probe(*ip, port, timeout))
    })
    .await
    .unwrap_or(false);
    (resolved, false, ok)
}

// ── Last-authority safety valve ────────────────────────────────────────

/// What the gate knows about this node's position in the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SafetyContext {
    /// This node authors blocks (always true for a GPN node — `force_mining`).
    pub is_authority: bool,
    /// Peers we are connected to, in either direction.
    pub connected_peers: usize,
    /// Known authority count when the chain exposes one; `0` = unknown.
    pub authority_count: usize,
    /// A solochain / `--dev` / `--alice` style local chain.
    pub dev_chain: bool,
}

/// Why the gate refused to hard-fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafetyReason {
    /// Nobody else is connected: we are the whole visible network.
    SoloChain,
    /// Dev/solo chain (never a real deployment).
    DevChain,
    /// The chain says this node is the only authority.
    OnlyAuthority,
    /// Exactly one peer: we are half of the visible network.
    TinyNetwork,
}

impl std::fmt::Display for SafetyReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SafetyReason::SoloChain => "this node is the only node on the network (0 peers connected)",
            SafetyReason::DevChain => "this is a dev/solo chain",
            SafetyReason::OnlyAuthority => "this node is the only authority on the chain",
            SafetyReason::TinyNetwork => "this node is one of only two nodes on the network",
        })
    }
}

/// Decide whether the hard gate must downgrade itself to soft mode.
///
/// The gate exists to keep undialable nodes *out of the mesh*, never to stop
/// the chain. When refusing to run would take the last authoring node (or a
/// dev chain) down, the safe action is always to keep running and warn.
pub fn safety_valve(ctx: &SafetyContext) -> Option<SafetyReason> {
    if !ctx.is_authority {
        return None;
    }
    if ctx.dev_chain {
        return Some(SafetyReason::DevChain);
    }
    if ctx.authority_count == 1 {
        return Some(SafetyReason::OnlyAuthority);
    }
    if ctx.connected_peers == 0 {
        return Some(SafetyReason::SoloChain);
    }
    if ctx.connected_peers == 1 {
        return Some(SafetyReason::TinyNetwork);
    }
    None
}

/// The prominent warning printed instead of exiting when the valve fires.
pub fn safety_valve_notice(reason: SafetyReason, detail: &str) -> String {
    format!(
        "\
================================================================================
  DIALABILITY GATE OVERRIDDEN - KEEPING THE CHAIN ALIVE
================================================================================
This node could not prove it is inbound-reachable:
  {detail}

Normally that is a hard failure (exit {EXIT_NOT_DIALABLE}). It is NOT applied here
because {reason}.

Exiting would stop block production and halt the chain - a far worse outage
than one node running in degraded (non-discoverable) mode. This node keeps
syncing, mining and serving jobs, and re-checks every 10 minutes.

Fix the forward/firewall anyway:
  https://gpncoin.ai/docs/run-a-node#make-your-node-dialable

To force the hard failure regardless:  GPN_DIALABILITY_SAFETY=0
================================================================================"
    )
}

/// `GPN_DIALABILITY_SAFETY` — default ON; an explicit false-y value lets the
/// gate exit even when it would stop the last authority.
pub fn parse_safety_enabled(raw: Option<&str>) -> bool {
    match raw.map(|s| s.trim().to_ascii_lowercase()) {
        Some(v) if matches!(v.as_str(), "0" | "false" | "no" | "off") => false,
        _ => true,
    }
}

/// `GPN_STUN_SERVER` — `off`/`0`/`no` disables the external STUN probe.
pub fn stun_server_from_env() -> Option<String> {
    match std::env::var("GPN_STUN_SERVER") {
        Ok(v) => {
            let v = v.trim().to_string();
            match v.to_ascii_lowercase().as_str() {
                "" => Some(DEFAULT_STUN_SERVER.to_string()),
                "0" | "false" | "no" | "off" => None,
                _ => Some(v),
            }
        }
        Err(_) => Some(DEFAULT_STUN_SERVER.to_string()),
    }
}

/// Gate configuration, resolved from the environment (+ `--public-addr`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateConfig {
    /// Whether the capability probe runs at all. Default: `true`.
    pub require: bool,
    /// Hard gate (`--require-backbone`, **DEFAULT**): exit
    /// [`EXIT_NOT_DIALABLE`] when the probe fails. Opt out with
    /// `--no-require-backbone` / `GPN_REQUIRE_BACKBONE=0` to run degraded.
    pub require_backbone: bool,
    /// Willing to be published as a seed (`GPN_SEED`, default `true`).
    /// Controls the operator notice only; publication is decided by peer
    /// witnesses, never by this flag.
    pub seed: bool,
    /// Announce the endpoint and accept inbound peers (`GPN_DISCOVERABLE`,
    /// default `true`). `false` implies `seed = false`.
    pub discoverable: bool,
    /// How long to wait for dialability evidence.
    pub timeout: Duration,
    /// The configured public address, if any.
    pub public_addr: Option<Multiaddr>,
    /// External STUN server for the active probe (`GPN_STUN_SERVER`).
    /// `None` disables the probe; the TCP hairpin probe still runs.
    pub stun_server: Option<String>,
    /// `GPN_DIALABILITY_SAFETY`: never let the gate stop the chain.
    pub safety: bool,
}

impl GateConfig {
    /// Resolve from the environment. `fallback_public` is
    /// `config.network.public_addresses` (i.e. `--public-addr`); the
    /// `GPN_PUBLIC_ADDR` env var wins over it.
    pub fn from_env(fallback_public: &[Multiaddr]) -> Self {
        let public_addr = std::env::var("GPN_PUBLIC_ADDR")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<Multiaddr>().ok())
            .or_else(|| fallback_public.first().cloned());

        let timeout = std::env::var("GPN_DIALABLE_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .map(|s| s.max(10))
            .unwrap_or(DEFAULT_TIMEOUT_SECS);

        let discoverable = parse_discoverable(std::env::var("GPN_DISCOVERABLE").ok().as_deref());
        let seed = discoverable && parse_seed(std::env::var("GPN_SEED").ok().as_deref());

        GateConfig {
            require: parse_require_dialable(std::env::var("GPN_REQUIRE_DIALABLE").ok().as_deref()),
            require_backbone: parse_require_backbone(
                std::env::var("GPN_REQUIRE_BACKBONE").ok().as_deref(),
            ),
            seed,
            discoverable,
            timeout: Duration::from_secs(timeout),
            public_addr,
            stun_server: stun_server_from_env(),
            safety: parse_safety_enabled(std::env::var("GPN_DIALABILITY_SAFETY").ok().as_deref()),
        }
    }

    /// Explicit constructor (used by tests / callers with resolved values).
    pub fn new(require: bool, timeout: Duration, public_addr: Option<Multiaddr>) -> Self {
        GateConfig {
            require,
            require_backbone: true,
            seed: true,
            discoverable: true,
            timeout,
            public_addr,
            stun_server: None,
            safety: true,
        }
    }

    /// The capability tier this configuration aims for (for logs/status).
    ///
    /// This is the *intent*; the honoured tier comes from peer witnesses
    /// (`gpn-liveness`), never from a self-reported flag.
    pub fn intended_tier(&self, dialable: bool) -> &'static str {
        if !self.discoverable {
            "private"
        } else if dialable {
            if self.seed {
                "seed-candidate"
            } else {
                "backbone"
            }
        } else {
            "private (degraded)"
        }
    }
}

/// Parse the `GPN_REQUIRE_DIALABLE` value. Default **ON**; only an explicit
/// false-y value opts out.
pub fn parse_require_dialable(raw: Option<&str>) -> bool {
    match raw.map(|s| s.trim().to_ascii_lowercase()) {
        Some(v) if matches!(v.as_str(), "0" | "false" | "no" | "off") => false,
        _ => true,
    }
}

/// Parse `GPN_SEED`. Default **ON**; an explicit false-y value opts out of
/// seed publication (and silences the degradation notice).
pub fn parse_seed(raw: Option<&str>) -> bool {
    match raw.map(|s| s.trim().to_ascii_lowercase()) {
        Some(v) if matches!(v.as_str(), "0" | "false" | "no" | "off") => false,
        _ => true,
    }
}

/// Parse `GPN_DISCOVERABLE`. Default **ON**.
pub fn parse_discoverable(raw: Option<&str>) -> bool {
    match raw.map(|s| s.trim().to_ascii_lowercase()) {
        Some(v) if matches!(v.as_str(), "0" | "false" | "no" | "off") => false,
        _ => true,
    }
}

/// Parse `GPN_REQUIRE_BACKBONE`. Default **ON** — a node that cannot be
/// dialled prints the §5.2 notice and exits [`EXIT_NOT_DIALABLE`].
///
/// Only an explicit false-y value (`0`/`false`/`no`/`off`) opts out into the
/// soft mode: run degraded, re-check every 10 minutes, print NOW DIALABLE on
/// promotion.
pub fn parse_require_backbone(raw: Option<&str>) -> bool {
    match raw.map(|s| s.trim().to_ascii_lowercase()) {
        Some(v) if matches!(v.as_str(), "0" | "false" | "no" | "off") => false,
        _ => true,
    }
}

/// Best-effort local LAN IPv4 address (no packet is sent: a UDP `connect` only
/// selects a route). Returns `None` when it cannot be determined — the caller
/// then prints a generic placeholder instead of failing.
pub fn detect_lan_ipv4() -> Option<IpAddr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    // TEST-NET-1: never routed, but enough for the kernel to pick a source IP.
    sock.connect("192.0.2.1:9").ok()?;
    let ip = sock.local_addr().ok()?.ip();
    match classify_ip(&ip) {
        IpClass::Private | IpClass::Public => Some(ip),
        _ => None,
    }
}

/// The listen port to advertise/probe, preferring the first listen address.
pub fn listen_port(listen: &[Multiaddr]) -> Option<u16> {
    listen.iter().find_map(|a| endpoint_of(a).map(|(_, p)| p))
}

/// The operator-facing degradation notice (design §5.2), or `None` when the
/// operator opted out of seed publication.
///
/// `hard_gate` selects the postamble: the **default** hard gate prints the
/// notice and exits with [`EXIT_NOT_DIALABLE`]; `--no-require-backbone` (soft
/// mode) keeps running as a non-discoverable participant and promises the
/// 10-minute re-check.
///
/// Emitted **once per degraded transition** (startup, or dialable → not
/// dialable), after the automatic NAT-mapping attempt in [`crate::portmap`] —
/// so a successful UPnP/NAT-PMP mapping never shows this notice — and never in
/// the per-block log loop.
pub fn degradation_notice(
    lan_ip: Option<IpAddr>,
    port: u16,
    seed: bool,
    hard_gate: bool,
) -> Option<String> {
    if !seed {
        return None;
    }
    let lan = lan_ip
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "<this host's LAN IP>".to_string());
    let status_line = if hard_gate {
        "This node will NOT start until it is dialable. Nothing else on this machine\n\
         is affected - fix the forward/firewall below and start it again."
    } else {
        "Your node is running and fully functional: it syncs, mines and serves AI jobs.\n\
         It continues in degraded (non-discoverable) mode."
    };
    let cgnat_tail = if hard_gate {
        "Run the node on a cheap VPS with a public IP."
    } else {
        "Run the node on a cheap VPS, or keep participating as-is.\n     This is a supported outcome, not an error."
    };
    let tail = if hard_gate {
        "This node will exit until it is dialable.\n\
         To run anyway (not recommended while the network is growing):\n\
         \x20 --no-require-backbone"
    } else {
        "This node re-checks every 10 minutes and will print NOW DIALABLE here when\n\
         the fix works. You can also restart to force a check.\n\n\
         Soft mode is active (--no-require-backbone / GPN_REQUIRE_BACKBONE=0).\n\
         Run private, silence:      --no-seed   (or GPN_SEED=0)"
    };
    Some(format!(
        "\
================================================================================
  YOUR NODE IS NOT DIALABLE
================================================================================
Other nodes cannot open a connection to this machine on TCP {port}.

{status_line}
What you lose: it is not listed as a backbone/seed node, so it earns no
liveness credit, gets no seed-list eligibility, and cannot be a bootstrap target.
Nothing is taken away from block rewards or job payouts.

To fix it (about 5 minutes):

  1. Find this machine's LAN IP. Detected: {lan}
     (Windows: ipconfig | Linux/macOS: ip addr or ifconfig)

  2. On your router, add a port-forwarding rule:
        Protocol ............ TCP
        External port ....... {port}
        Internal port ....... {port}
        Internal IP/host .... {lan}
     Router admin is usually http://192.168.1.1 or http://192.168.0.1.

     Recommended setup (EXAMPLE - these are example values, not detected):
       - Give this machine a DHCP reservation so its LAN IP never changes
         (example: {example_lan}).
       - Forward exactly one TCP port, external = internal ({port}).
       - Example router rule:  TCP {port} -> {example_lan}:{port}
     Detected on this machine: LAN {lan}, port {port}.

  3. Allow the port in this machine's firewall.
     Windows (Administrator PowerShell):
       New-NetFirewallRule -DisplayName \"GPN node\" -Direction Inbound `
         -Protocol TCP -LocalPort {port} -Action Allow
     Linux (ufw):
       sudo ufw allow {port}/tcp
     Linux (firewalld):
       sudo firewall-cmd --add-port={port}/tcp --permanent && sudo firewall-cmd --reload

  4. UPnP/NAT-PMP: the node already tried this automatically (GPN_UPNP=auto is
     the default) and keeps retrying every 10 minutes - just enable UPnP on
     your router and it is picked up without a restart. Or use step 2.
     Disable the attempt with --no-upnp / GPN_UPNP=off.

  5. CGNAT: if your router's WAN address is in 100.64.0.0/10, no port forward
     can work. {cgnat_tail}

Full guide with per-OS detail:
  https://gpncoin.ai/docs/run-a-node#make-your-node-dialable

{tail}
================================================================================",
        example_lan = RECOMMENDED_LAN_EXAMPLE,
    ))
}

/// The one-time promotion block (design §5.3), printed on the
/// `not dialable → dialable` transition only.
pub fn promotion_notice(port: u16) -> String {
    format!(
        "\
================================================================================
  NOW DIALABLE - your node is reachable on TCP {port}
================================================================================
Inbound reachability was confirmed. This node now announces itself as a backbone
node and is eligible for seed listing. Nothing else to do.

(This message is shown once.)
================================================================================"
    )
}

/// Edge-triggered state change between two dialability observations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialabilityChange {
    /// State unchanged — print nothing (never nag).
    None,
    /// `not dialable → dialable`: print [`promotion_notice`] once.
    BecameDialable,
    /// `dialable → not dialable`: print [`degradation_notice`] once.
    BecameUndialable,
}

/// Pure edge detector (design §5.3: edge-triggered, not level-triggered).
pub fn edge_transition(last_dialable: bool, now_dialable: bool) -> DialabilityChange {
    match (last_dialable, now_dialable) {
        (false, true) => DialabilityChange::BecameDialable,
        (true, false) => DialabilityChange::BecameUndialable,
        _ => DialabilityChange::None,
    }
}

/// Periodic re-check loop (design §5.3): every [`RECHECK_SECS`], re-evaluate
/// inbound reachability and print the edge-triggered notice exactly once per
/// transition. `initial_dialable` is the startup verdict, so the watcher does
/// not repeat a notice the startup path already printed.
///
/// Returns immediately if the probe is disabled. Runs forever otherwise.
pub async fn watch_dialability<N>(
    network: std::sync::Arc<N>,
    cfg: GateConfig,
    initial_dialable: bool,
) where
    N: NetworkStatusProvider + NetworkStateInfo + Send + Sync + 'static + ?Sized,
{
    if !cfg.require {
        return;
    }
    let mut last_dialable = initial_dialable;
    let mut tick = tokio::time::interval(Duration::from_secs(RECHECK_SECS));
    tick.tick().await; // consume the immediate first tick
    loop {
        tick.tick().await;
        let inbound = match network.network_state().await {
            Ok(state) => inbound_peer_count(&state),
            Err(_) => 0,
        };
        let listen = network.listen_addresses();
        let observed = network.external_addresses();
        let verdict = full_verdict(
            inbound,
            &listen,
            &observed,
            cfg.public_addr.as_ref(),
            cfg.stun_server.as_deref(),
        )
        .await;
        let port = listen_port(&listen).unwrap_or(DEFAULT_SEED_PORT);

        match edge_transition(last_dialable, verdict.dialable) {
            DialabilityChange::None => {}
            DialabilityChange::BecameDialable => {
                last_dialable = true;
                log::info!(
                    "dialability: inbound reachability confirmed ({})",
                    verdict.detail
                );
                for line in promotion_notice(port).lines() {
                    log::info!("{line}");
                }
            }
            DialabilityChange::BecameUndialable => {
                last_dialable = false;
                log::warn!(
                    "dialability: lost inbound reachability ({})",
                    verdict.detail
                );
                if let Some(notice) = degradation_notice(detect_lan_ipv4(), port, cfg.seed, false) {
                    for line in notice.lines() {
                        log::warn!("{line}");
                    }
                }
            }
        }
    }
}

/// Resolve the configured DNS name (if any), run the active probes, and
/// evaluate.
///
/// Kept separate from [`evaluate_with`] so the decision stays pure and every
/// network-touching step is explicit.
async fn full_verdict(
    inbound: usize,
    listen: &[Multiaddr],
    observed: &[Multiaddr],
    configured: Option<&Multiaddr>,
    stun_server: Option<&str>,
) -> Verdict {
    // Passive evidence is always tried FIRST and for free. Only when it fails
    // do we pay for DNS + STUN + a TCP connect.
    let passive = evaluate(inbound, listen, observed, configured);
    if passive.dialable {
        return passive;
    }
    let Some(cfg_ma) = configured else {
        return passive;
    };
    let (resolved, stun_mapped, tcp_probe_ok) = probe_configured_endpoint(cfg_ma, stun_server).await;
    evaluate_with(&GateInputs {
        inbound_peers: inbound,
        listen,
        observed,
        configured,
        resolved: &resolved,
        stun_mapped,
        tcp_probe_ok,
    })
}

/// Wait until the node is demonstrably dialable, or the grace window expires.
///
/// Polls the live network every [`POLL_SECS`] seconds and evaluates
/// [`full_verdict`] (passive evidence first, then DNS + the active probes).
/// Returns the final verdict (never blocks longer than `cfg.timeout`).
pub async fn wait_for_dialability<N>(network: &N, cfg: &GateConfig) -> Verdict
where
    N: NetworkStatusProvider + NetworkStateInfo,
{
    let started = Instant::now();
    let deadline = started + cfg.timeout;
    let mut last_progress = started;
    let mut verdict;
    // Active probes are expensive (DNS + a 3 s STUN round trip + up to 5 s of
    // TCP connect), so they are cached and retried at most every
    // [`PROBE_REFRESH_SECS`] instead of once per 5 s poll.
    let mut cached_probe: Option<(Instant, Vec<IpAddr>, bool, bool)> = None;

    loop {
        let inbound = match network.network_state().await {
            Ok(state) => inbound_peer_count(&state),
            Err(_) => 0,
        };
        let listen = network.listen_addresses();
        let observed = network.external_addresses();

        // Passive evidence first — free, and it needs no network traffic.
        verdict = evaluate(inbound, &listen, &observed, cfg.public_addr.as_ref());
        if !verdict.dialable {
            if let Some(cfg_ma) = cfg.public_addr.as_ref() {
                let stale = cached_probe
                    .as_ref()
                    .map(|(at, _, _, _)| {
                        at.elapsed() >= Duration::from_secs(PROBE_REFRESH_SECS)
                    })
                    .unwrap_or(true);
                if stale {
                    let (resolved, stun_mapped, tcp_probe_ok) =
                        probe_configured_endpoint(cfg_ma, cfg.stun_server.as_deref()).await;
                    cached_probe = Some((Instant::now(), resolved, stun_mapped, tcp_probe_ok));
                }
                if let Some((_, resolved, stun_mapped, tcp_probe_ok)) = cached_probe.as_ref() {
                    verdict = evaluate_with(&GateInputs {
                        inbound_peers: inbound,
                        listen: &listen,
                        observed: &observed,
                        configured: cfg.public_addr.as_ref(),
                        resolved,
                        stun_mapped: *stun_mapped,
                        tcp_probe_ok: *tcp_probe_ok,
                    });
                }
            }
        }

        if verdict.dialable || Instant::now() >= deadline {
            return verdict;
        }

        if last_progress.elapsed() >= Duration::from_secs(PROGRESS_EVERY_SECS) {
            last_progress = Instant::now();
            log::info!(
                "dialability: waiting for inbound reachability ({:.0}s/{:.0}s) — \
                 inbound_peers={inbound} listening=[{}] observed=[{}]",
                started.elapsed().as_secs_f64(),
                cfg.timeout.as_secs_f64(),
                join(&listen),
                join(&observed),
            );
        }

        let sleep = POLL_SECS.min(
            deadline
                .saturating_duration_since(Instant::now())
                .as_secs()
                .max(1),
        );
        tokio::time::sleep(Duration::from_secs(sleep)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ── address classification ────────────────────────────────────────────

    #[test]
    fn public_v4_is_public() {
        for s in ["8.8.8.8", "1.2.3.4", "23.241.109.255", "203.0.113.9"] {
            let ip: IpAddr = s.parse().unwrap();
            if s == "203.0.113.9" {
                // TEST-NET-3 is documentation space — intentionally NOT public.
                assert_eq!(classify_ip(&ip), IpClass::Private, "{s}");
            } else {
                assert_eq!(classify_ip(&ip), IpClass::Public, "{s}");
            }
        }
    }

    #[test]
    fn rfc1918_is_private() {
        for s in ["10.0.0.1", "172.16.3.4", "192.168.1.5", "100.64.7.9"] {
            let ip: IpAddr = s.parse().unwrap();
            assert_eq!(classify_ip(&ip), IpClass::Private, "{s}");
        }
    }

    #[test]
    fn loopback_link_local_unspecified() {
        assert_eq!(
            classify_ip(&IpAddr::V4(Ipv4Addr::LOCALHOST)),
            IpClass::Loopback
        );
        assert_eq!(
            classify_ip(&IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
            IpClass::Unspecified
        );
        assert_eq!(
            classify_ip(&"169.254.10.20".parse().unwrap()),
            IpClass::LinkLocal
        );
        assert_eq!(
            classify_ip(&IpAddr::V6(Ipv6Addr::LOCALHOST)),
            IpClass::Loopback
        );
        assert_eq!(
            classify_ip(&IpAddr::V6(Ipv6Addr::UNSPECIFIED)),
            IpClass::Unspecified
        );
        assert_eq!(classify_ip(&"fe80::1".parse().unwrap()), IpClass::LinkLocal);
        assert_eq!(classify_ip(&"fd00::1".parse().unwrap()), IpClass::Private);
        assert_eq!(
            classify_ip(&"2001:4860:4860::8888".parse().unwrap()),
            IpClass::Public
        );
        assert!(!IpClass::Private.is_public());
        assert!(IpClass::Public.is_public());
    }

    #[test]
    fn endpoint_extraction_ignores_p2p_suffix() {
        let ma: Multiaddr =
            "/ip4/1.2.3.4/tcp/30333/p2p/12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN"
                .parse()
                .unwrap();
        assert_eq!(
            endpoint_of(&ma),
            Some((IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4)), 30333))
        );
        let no_port: Multiaddr = "/ip4/1.2.3.4".parse().unwrap();
        assert_eq!(endpoint_of(&no_port), None);
    }

    // ── gate decision ─────────────────────────────────────────────────────

    fn ma(s: &str) -> Multiaddr {
        s.parse().unwrap()
    }

    #[test]
    fn inbound_peer_passes_regardless_of_addresses() {
        let v = evaluate(1, &[ma("/ip4/0.0.0.0/tcp/30333")], &[], None);
        assert!(v.dialable);
        assert_eq!(v.evidence, Some(Evidence::InboundPeer));
    }

    #[test]
    fn nat_node_without_forward_fails() {
        // Listening on 0.0.0.0, never observed, no inbound peer.
        let v = evaluate(0, &[ma("/ip4/0.0.0.0/tcp/30333")], &[], None);
        assert!(!v.dialable);
        assert!(v.evidence.is_none());
    }

    #[test]
    fn loopback_only_fails() {
        let v = evaluate(
            0,
            &[ma("/ip4/127.0.0.1/tcp/30333")],
            &[ma("/ip4/127.0.0.1/tcp/40000")],
            None,
        );
        assert!(!v.dialable);
    }

    #[test]
    fn public_bind_passes_without_config() {
        let v = evaluate(0, &[ma("/ip4/23.241.109.255/tcp/30333")], &[], None);
        assert!(v.dialable);
        assert_eq!(v.evidence, Some(Evidence::PublicListenAddr));
    }

    #[test]
    fn configured_addr_must_match_exactly() {
        let cfg = ma("/ip4/1.2.3.4/tcp/30333");
        let pass = evaluate(
            0,
            &[ma("/ip4/0.0.0.0/tcp/30333")],
            &[ma("/ip4/1.2.3.4/tcp/30333")],
            Some(&cfg),
        );
        assert!(pass.dialable);
        assert_eq!(pass.evidence, Some(Evidence::ConfiguredAddrObserved));

        // Right IP, wrong port → the forward is not open on the configured port.
        let fail = evaluate(
            0,
            &[ma("/ip4/0.0.0.0/tcp/30333")],
            &[ma("/ip4/1.2.3.4/tcp/54321")],
            Some(&cfg),
        );
        assert!(!fail.dialable);
    }

    #[test]
    fn configured_private_addr_fails() {
        let cfg = ma("/ip4/192.168.1.5/tcp/30333");
        let v = evaluate(0, &[ma("/ip4/192.168.1.5/tcp/30333")], &[], Some(&cfg));
        assert!(!v.dialable);
        assert!(v.detail.contains("NON-public"), "detail: {}", v.detail);
    }

    // ── /dns4 public addresses (fix #1) ──────────────────────────────────

    #[test]
    fn host_port_of_accepts_dns_names_and_ips() {
        assert_eq!(
            host_port_of(&ma(
                "/dns4/gpn-testnet.duckdns.org/tcp/30333/\
                 p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo"
            )),
            Some((Host::Dns("gpn-testnet.duckdns.org".into()), 30333))
        );
        assert_eq!(
            host_port_of(&ma("/dns6/node.example/tcp/30333")),
            Some((Host::Dns("node.example".into()), 30333))
        );
        assert_eq!(
            host_port_of(&ma("/ip4/1.2.3.4/tcp/30333")),
            Some((Host::Ip("1.2.3.4".parse().unwrap()), 30333))
        );
        // No port / no host → not an endpoint.
        assert_eq!(host_port_of(&ma("/ip4/1.2.3.4")), None);
        assert_eq!(host_port_of(&ma("/dns4/example.org")), None);
    }

    #[test]
    fn dns4_public_addr_passes_when_a_resolved_ip_is_observed() {
        let cfg = ma("/dns4/gpn-testnet.duckdns.org/tcp/30333");
        let resolved: Vec<IpAddr> = vec!["142.129.54.88".parse().unwrap()];
        let observed = vec![ma("/ip4/142.129.54.88/tcp/30333")];
        let v = evaluate_with(&GateInputs {
            inbound_peers: 0,
            listen: &[ma("/ip4/0.0.0.0/tcp/30333")],
            observed: &observed,
            configured: Some(&cfg),
            resolved: &resolved,
            stun_mapped: false,
            tcp_probe_ok: false,
        });
        assert!(v.dialable, "detail: {}", v.detail);
        assert_eq!(v.evidence, Some(Evidence::ConfiguredAddrObserved));
        assert!(v.detail.contains("resolves to 142.129.54.88"), "{}", v.detail);
    }

    #[test]
    fn dns4_is_no_longer_rejected_as_unusable() {
        // The 2026-10-20 regression: a /dns4 GPN_PUBLIC_ADDR failed with
        // "not a usable /ip4|ip6/<addr>/tcp/<port> multiaddress".
        let cfg = ma("/dns4/gpn-testnet.duckdns.org/tcp/30333");
        let resolved: Vec<IpAddr> = vec!["142.129.54.88".parse().unwrap()];
        let v = evaluate_with(&GateInputs {
            inbound_peers: 0,
            listen: &[ma("/ip4/0.0.0.0/tcp/30333")],
            observed: &[],
            configured: Some(&cfg),
            resolved: &resolved,
            stun_mapped: false,
            tcp_probe_ok: false,
        });
        assert!(!v.dialable);
        assert!(
            !v.detail.contains("not a usable"),
            "dns4 must be accepted as a shape: {}",
            v.detail
        );
        assert!(v.detail.contains("142.129.54.88"), "{}", v.detail);
    }

    #[test]
    fn dns4_resolving_to_a_private_ip_fails() {
        let cfg = ma("/dns4/router.lan/tcp/30333");
        let resolved: Vec<IpAddr> = vec!["192.168.1.5".parse().unwrap()];
        let v = evaluate_with(&GateInputs {
            inbound_peers: 0,
            listen: &[ma("/ip4/0.0.0.0/tcp/30333")],
            observed: &[],
            configured: Some(&cfg),
            resolved: &resolved,
            stun_mapped: true,
            tcp_probe_ok: true,
        });
        assert!(!v.dialable, "a private resolution is never dialable");
        assert!(v.detail.contains("NON-public"), "{}", v.detail);
    }

    #[test]
    fn dns4_that_does_not_resolve_fails_with_a_clear_reason() {
        let cfg = ma("/dns4/nonexistent.invalid/tcp/30333");
        let v = evaluate_with(&GateInputs::new(
            0,
            &[ma("/ip4/0.0.0.0/tcp/30333")],
            &[],
            Some(&cfg),
        ));
        assert!(!v.dialable);
        assert!(v.detail.contains("did not"), "{}", v.detail);
    }

    // ── active evidence: tcp probe / STUN (fix #2) ───────────────────────

    #[test]
    fn tcp_probe_ok_is_accepted_as_evidence() {
        let cfg = ma("/ip4/142.129.54.88/tcp/30333");
        let v = evaluate_with(&GateInputs {
            inbound_peers: 0,
            listen: &[ma("/ip4/0.0.0.0/tcp/30333")],
            observed: &[],
            configured: Some(&cfg),
            resolved: &[],
            stun_mapped: false,
            tcp_probe_ok: true,
        });
        assert!(v.dialable, "detail: {}", v.detail);
        assert_eq!(v.evidence, Some(Evidence::TcpProbeOk));
    }

    #[test]
    fn stun_mapping_is_accepted_and_preferred_over_the_tcp_probe() {
        let cfg = ma("/ip4/142.129.54.88/tcp/30333");
        let v = evaluate_with(&GateInputs {
            inbound_peers: 0,
            listen: &[ma("/ip4/0.0.0.0/tcp/30333")],
            observed: &[],
            configured: Some(&cfg),
            resolved: &[],
            stun_mapped: true,
            tcp_probe_ok: true,
        });
        assert!(v.dialable);
        assert_eq!(v.evidence, Some(Evidence::StunMappedAddr));
    }

    #[test]
    fn passive_evidence_beats_a_failed_probe_and_vice_versa() {
        // Configured address observed, probes failed → still dialable.
        let cfg = ma("/ip4/1.2.3.4/tcp/30333");
        let observed = vec![ma("/ip4/1.2.3.4/tcp/30333")];
        let v = evaluate_with(&GateInputs {
            inbound_peers: 0,
            listen: &[ma("/ip4/0.0.0.0/tcp/30333")],
            observed: &observed,
            configured: Some(&cfg),
            resolved: &[],
            stun_mapped: false,
            tcp_probe_ok: false,
        });
        assert_eq!(v.evidence, Some(Evidence::ConfiguredAddrObserved));

        // Nothing at all → fail, and the failure names every evidence source.
        let v = evaluate_with(&GateInputs::new(
            0,
            &[ma("/ip4/0.0.0.0/tcp/30333")],
            &[],
            Some(&cfg),
        ));
        assert!(!v.dialable);
        assert!(v.detail.contains("STUN"), "{}", v.detail);
        assert!(v.detail.contains("TCP"), "{}", v.detail);
    }

    #[test]
    fn stun_binding_request_is_well_formed() {
        let req = build_stun_binding_request();
        assert_eq!(req.len(), 20);
        assert_eq!(u16::from_be_bytes([req[0], req[1]]), 0x0001);
        assert_eq!(u16::from_be_bytes([req[2], req[3]]), 0);
        assert_eq!(&req[4..8], &[0x21, 0x12, 0xA4, 0x42]);
        // Two requests never share a transaction id.
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert_ne!(build_stun_binding_request()[8..16], req[8..16]);
    }

    #[test]
    fn stun_xor_mapped_address_is_parsed() {
        let tid = [9u8; 12];
        let mut msg = vec![0x01, 0x01, 0x00, 0x0C];
        msg.extend_from_slice(&[0x21, 0x12, 0xA4, 0x42]);
        msg.extend_from_slice(&tid);
        // XOR-MAPPED-ADDRESS, 8 bytes: family IPv4 + port ^ 0x2112 + ip ^ cookie.
        msg.extend_from_slice(&[0x00, 0x20, 0x00, 0x08, 0x00, 0x01]);
        let port: u16 = 30333;
        msg.extend_from_slice(&(port ^ 0x2112).to_be_bytes());
        let ip = [142u8, 129, 54, 88];
        let cookie = [0x21u8, 0x12, 0xA4, 0x42];
        for i in 0..4 {
            msg.push(ip[i] ^ cookie[i]);
        }
        assert_eq!(
            parse_stun_xor_mapped(&msg),
            Some(("142.129.54.88".parse().unwrap(), 30333))
        );
    }

    #[test]
    fn stun_parser_rejects_garbage_and_non_responses() {
        assert_eq!(parse_stun_xor_mapped(&[]), None);
        assert_eq!(parse_stun_xor_mapped(&[0u8; 20]), None);
        // Binding *request* is not a response.
        assert_eq!(parse_stun_xor_mapped(&build_stun_binding_request()), None);
        // Wrong magic cookie.
        let mut bad = vec![0x01, 0x01, 0x00, 0x00, 0, 0, 0, 0];
        bad.extend_from_slice(&[0u8; 12]);
        assert_eq!(parse_stun_xor_mapped(&bad), None);
        // Truncated attribute.
        let mut trunc = vec![0x01, 0x01, 0x00, 0x08];
        trunc.extend_from_slice(&[0x21, 0x12, 0xA4, 0x42]);
        trunc.extend_from_slice(&[0u8; 12]);
        trunc.extend_from_slice(&[0x00, 0x20, 0x00, 0x08, 0x00, 0x01]);
        assert_eq!(parse_stun_xor_mapped(&trunc), None);
    }

    // ── last-authority safety valve (fix #3) ─────────────────────────────

    fn safety(is_authority: bool, peers: usize, authorities: usize, dev: bool) -> SafetyContext {
        SafetyContext {
            is_authority,
            connected_peers: peers,
            authority_count: authorities,
            dev_chain: dev,
        }
    }

    #[test]
    fn safety_valve_fires_on_the_cases_that_halt_the_chain() {
        assert_eq!(
            safety_valve(&safety(true, 0, 0, false)),
            Some(SafetyReason::SoloChain)
        );
        assert_eq!(
            safety_valve(&safety(true, 0, 0, true)),
            Some(SafetyReason::DevChain)
        );
        assert_eq!(
            safety_valve(&safety(true, 3, 1, false)),
            Some(SafetyReason::OnlyAuthority)
        );
        assert_eq!(
            safety_valve(&safety(true, 1, 0, false)),
            Some(SafetyReason::TinyNetwork)
        );
    }

    #[test]
    fn safety_valve_does_not_fire_on_a_healthy_multi_node_network() {
        assert_eq!(safety_valve(&safety(true, 2, 0, false)), None);
        assert_eq!(safety_valve(&safety(true, 25, 3, false)), None);
        // A non-authority node can always be refused: it cannot halt the chain.
        assert_eq!(safety_valve(&safety(false, 0, 0, false)), None);
    }

    #[test]
    fn safety_valve_notice_is_prominent_and_ascii() {
        let note = safety_valve_notice(SafetyReason::SoloChain, "no evidence");
        assert!(note.contains("KEEPING THE CHAIN ALIVE"), "{note}");
        assert!(note.contains("no evidence"), "{note}");
        assert!(note.contains("only node on the network"), "{note}");
        assert!(note.contains("GPN_DIALABILITY_SAFETY=0"), "{note}");
        assert!(note.is_ascii(), "Windows consoles: notice must be ASCII only");
    }

    #[test]
    fn safety_toggle_defaults_on_and_can_be_disabled() {
        assert!(parse_safety_enabled(None));
        assert!(parse_safety_enabled(Some("")));
        assert!(parse_safety_enabled(Some("1")));
        for v in ["0", "false", "FALSE", "no", "off", " 0 "] {
            assert!(!parse_safety_enabled(Some(v)), "{v} must disable the valve");
        }
    }

    #[test]
    fn stun_env_defaults_to_google_and_can_be_disabled() {
        // RAII-free: these tests own their process env keys only by convention;
        // a wrong value here would still leave the probe harmless.
        std::env::remove_var("GPN_STUN_SERVER");
        assert_eq!(stun_server_from_env().as_deref(), Some(DEFAULT_STUN_SERVER));
        std::env::set_var("GPN_STUN_SERVER", "off");
        assert_eq!(stun_server_from_env(), None);
        std::env::set_var("GPN_STUN_SERVER", "stun.example.org:3478");
        assert_eq!(
            stun_server_from_env().as_deref(),
            Some("stun.example.org:3478")
        );
        std::env::remove_var("GPN_STUN_SERVER");
    }

    #[test]
    fn observed_public_on_listen_port_passes_without_config() {
        let v = evaluate(
            0,
            &[ma("/ip4/0.0.0.0/tcp/30333")],
            &[ma("/ip4/1.2.3.4/tcp/30333")],
            None,
        );
        assert!(v.dialable);
        assert_eq!(v.evidence, Some(Evidence::ObservedPublicAtListenPort));

        // Different port → ephemeral NAT mapping, not dialable.
        let v = evaluate(
            0,
            &[ma("/ip4/0.0.0.0/tcp/30333")],
            &[ma("/ip4/1.2.3.4/tcp/54321")],
            None,
        );
        assert!(!v.dialable);
    }

    // ── require / opt-out parsing ─────────────────────────────────────────

    #[test]
    fn require_dialable_defaults_on() {
        assert!(parse_require_dialable(None));
        assert!(parse_require_dialable(Some("1")));
        assert!(parse_require_dialable(Some("true")));
        assert!(parse_require_dialable(Some("")));
        assert!(parse_require_dialable(Some("yes")));
    }

    #[test]
    fn require_dialable_can_be_opted_out() {
        for v in ["0", "false", "FALSE", "no", "off", " 0 "] {
            assert!(!parse_require_dialable(Some(v)), "{v} should opt out");
        }
    }

    // ── v2 membership: seed-by-default, soft gate ─────────────────────────

    #[test]
    fn seed_and_discoverable_default_on_and_opt_out() {
        assert!(parse_seed(None));
        assert!(parse_seed(Some("1")));
        assert!(parse_seed(Some("")));
        for v in ["0", "false", "FALSE", "no", "off", " 0 "] {
            assert!(!parse_seed(Some(v)), "{v} should opt out of seed");
        }
        assert!(parse_discoverable(None));
        for v in ["0", "false", "no", "off"] {
            assert!(!parse_discoverable(Some(v)), "{v} should opt out");
        }
    }

    #[test]
    fn require_backbone_is_on_by_default() {
        assert!(parse_require_backbone(None));
        assert!(parse_require_backbone(Some("")));
        assert!(parse_require_backbone(Some("1")));
        assert!(parse_require_backbone(Some("on")));
        // Explicit opt-out into soft mode.
        for v in ["0", "false", "FALSE", "no", "off", " 0 "] {
            assert!(!parse_require_backbone(Some(v)), "{v} must opt out");
        }
    }

    #[test]
    fn no_discoverable_implies_no_seed() {
        let mut cfg = GateConfig::new(true, Duration::from_secs(10), None);
        cfg.discoverable = false;
        cfg.seed = false;
        assert_eq!(cfg.intended_tier(true), "private");
    }

    #[test]
    fn intended_tier_labels_dialable_and_degraded() {
        let mut cfg = GateConfig::new(true, Duration::from_secs(10), None);
        assert_eq!(cfg.intended_tier(true), "seed-candidate");
        assert_eq!(cfg.intended_tier(false), "private (degraded)");
        cfg.seed = false;
        assert_eq!(cfg.intended_tier(true), "backbone");
    }

    #[test]
    fn degradation_notice_is_actionable_and_suppressed_by_no_seed() {
        let lan: IpAddr = "192.168.1.42".parse().unwrap();
        let note = degradation_notice(Some(lan), 30333, true, true).expect("notice when seeding");
        assert!(note.contains("YOUR NODE IS NOT DIALABLE"), "{note}");
        assert!(
            note.contains("192.168.1.42"),
            "LAN IP must be filled in: {note}"
        );
        assert!(note.contains("External port ....... 30333"), "port: {note}");
        assert!(
            note.contains("--no-require-backbone"),
            "must document the opt-out: {note}"
        );
        assert!(
            note.contains("This node will exit until it is dialable"),
            "hard mode must say it exits: {note}"
        );
        assert!(
            note.contains("\n  --no-require-backbone\n"),
            "the opt-out must be indented as a runnable flag: {note}"
        );
        assert!(note.contains("100.64.0.0/10"), "must explain CGNAT: {note}");
        assert!(
            note.contains("UPnP/NAT-PMP") && note.contains("--no-upnp"),
            "step 4 must reflect that mapping was attempted automatically: {note}"
        );
        assert!(
            note.contains("ufw allow 30333/tcp"),
            "must show the firewall step: {note}"
        );
        assert!(
            note.contains("Recommended setup (EXAMPLE"),
            "must include the recommended example block: {note}"
        );
        assert!(
            note.contains("TCP 30333 -> 192.168.1.50:30333"),
            "must show a concrete example router rule: {note}"
        );
        assert!(
            note.is_ascii(),
            "Windows consoles: notice must be ASCII only"
        );

        // Unknown LAN IP still yields a usable block (placeholder, not a panic).
        let generic = degradation_notice(None, 30333, true, true).unwrap();
        assert!(generic.contains("LAN IP"), "{generic}");

        // The operator asked to be private: never nag.
        assert!(degradation_notice(Some(lan), 30333, false, true).is_none());
        assert!(degradation_notice(Some(lan), 30333, false, false).is_none());
    }

    #[test]
    fn soft_mode_notice_promises_recheck_and_continues() {
        let lan: IpAddr = "192.168.1.42".parse().unwrap();
        let note = degradation_notice(Some(lan), 30333, true, false).expect("notice in soft mode");
        assert!(note.contains("re-checks every 10 minutes"), "{note}");
        assert!(
            !note.contains("This node will exit"),
            "soft mode must not claim exit: {note}"
        );
        assert!(note.contains("--no-require-backbone"), "{note}");
        assert!(
            note.is_ascii(),
            "Windows consoles: notice must be ASCII only"
        );
    }

    #[test]
    fn promotion_notice_is_exact_and_ascii() {
        let note = promotion_notice(30333);
        assert!(
            note.contains("NOW DIALABLE - your node is reachable on TCP 30333"),
            "{note}"
        );
        assert!(note.contains("eligib"));
        assert!(note.contains("(This message is shown once.)"), "{note}");
        assert!(
            note.is_ascii(),
            "Windows consoles: notice must be ASCII only"
        );
    }

    #[test]
    fn edge_transitions_fire_once_per_change() {
        // Steady state prints nothing (edge-triggered, never level-triggered).
        assert_eq!(edge_transition(false, false), DialabilityChange::None);
        assert_eq!(edge_transition(true, true), DialabilityChange::None);
        // not dialable -> dialable promotes exactly once.
        assert_eq!(
            edge_transition(false, true),
            DialabilityChange::BecameDialable
        );
        // A later loss of reachability degrades exactly once (and can re-promote).
        assert_eq!(
            edge_transition(true, false),
            DialabilityChange::BecameUndialable
        );
        assert_eq!(
            edge_transition(false, true),
            DialabilityChange::BecameDialable
        );
    }

    #[test]
    fn listen_port_prefers_the_listen_address() {
        assert_eq!(listen_port(&[ma("/ip4/0.0.0.0/tcp/30333")]), Some(30333));
        assert_eq!(listen_port(&[]), None);
    }

    // ── wait loop with a stub network ─────────────────────────────────────

    struct StubNet {
        inbound: usize,
        listen: Vec<Multiaddr>,
        observed: Vec<Multiaddr>,
        polls: AtomicUsize,
    }

    impl StubNet {
        fn new(inbound: usize, listen: Vec<Multiaddr>, observed: Vec<Multiaddr>) -> Self {
            StubNet {
                inbound,
                listen,
                observed,
                polls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl NetworkStatusProvider for StubNet {
        async fn status(&self) -> Result<sc_network::NetworkStatus, ()> {
            Err(())
        }
        async fn network_state(&self) -> Result<NetworkState, ()> {
            self.polls.fetch_add(1, Ordering::SeqCst);
            let mut connected_peers = std::collections::HashMap::new();
            for i in 0..self.inbound {
                connected_peers.insert(
                    format!("peer{i}"),
                    sc_network::network_state::Peer {
                        endpoint: PeerEndpoint::Listening {
                            local_addr: ma("/ip4/10.0.0.1/tcp/30333").into(),
                            send_back_addr: ma("/ip4/10.0.0.9/tcp/4000").into(),
                        },
                        version_string: None,
                        latest_ping_time: None,
                        known_addresses: Default::default(),
                    },
                );
            }
            Ok(NetworkState {
                peer_id: "local".into(),
                listened_addresses: Default::default(),
                external_addresses: Default::default(),
                connected_peers,
                not_connected_peers: Default::default(),
                peerset: serde_json::Value::Null,
            })
        }
    }

    impl NetworkStateInfo for StubNet {
        fn external_addresses(&self) -> Vec<Multiaddr> {
            self.observed.clone()
        }
        fn listen_addresses(&self) -> Vec<Multiaddr> {
            self.listen.clone()
        }
        fn local_peer_id(&self) -> sc_network::PeerId {
            "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN"
                .parse()
                .expect("valid peer id")
        }
    }

    #[tokio::test]
    async fn stub_gate_passes_on_inbound_peer() {
        let net = StubNet::new(1, vec![ma("/ip4/0.0.0.0/tcp/30333")], vec![]);
        let cfg = GateConfig::new(true, Duration::from_secs(10), None);
        let v = wait_for_dialability(&net, &cfg).await;
        assert!(v.dialable);
        assert_eq!(v.evidence, Some(Evidence::InboundPeer));
        assert_eq!(
            net.polls.load(Ordering::SeqCst),
            1,
            "should return on first poll"
        );
    }

    #[tokio::test]
    async fn stub_gate_fails_at_timeout_without_evidence() {
        let net = StubNet::new(0, vec![ma("/ip4/0.0.0.0/tcp/30333")], vec![]);
        // Zero timeout: evaluate once, then fail immediately.
        let cfg = GateConfig::new(true, Duration::ZERO, None);
        let v = wait_for_dialability(&net, &cfg).await;
        assert!(!v.dialable);
        assert!(v.evidence.is_none());
    }
}
