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
//! # `/dns4`, `/ip6` and `/dns6` are first-class public addresses
//!
//! `GPN_PUBLIC_ADDR=/dns4/gpn-testnet.duckdns.org/tcp/30333` is accepted: the
//! name is **resolved at check time** and *every* resolved IP must classify as
//! public. Matching against peer observations is done on the **resolved IPs**,
//! never on the literal hostname (peers only ever report IPs).
//!
//! # IPv6 (owner decision 2026-11-02: v6 is the answer to CGNAT)
//!
//! Carrier-grade NAT (`100.64.0.0/10`) makes the IPv4 gate unpassable for home
//! broadband operators. IPv6 has no NAT: when the ISP routes a global prefix,
//! the only remaining gate is the router **firewall** (a pinhole, not a port
//! forward). [`detect_public_ipv6`] enumerates interfaces and accepts **only**
//! global unicast `2000::/3`, rejecting link-local, ULA, loopback, unspecified,
//! IPv4-mapped and documentation addresses. It prefers a stable address over an
//! RFC 4941 temporary one and prefers the interface carrying the default route.
//! The listener gains a `/ip6/::/tcp/<port>` bind and the published endpoint set
//! gains `/ip6/<addr>/tcp/<port>` **in addition to** the IPv4 entry. The
//! evidence model is identical for both stacks (inbound peer, observed
//! address, STUN, TCP probe) — v6 never gets a weaker path. Prefix rotation is
//! re-detected on the 10-minute loop and republished, dropping the stale
//! address.
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
//! # Reachability tiers (SPEC 47)
//!
//! Three tiers exist, and only the first is expected to be a minority of the
//! fleet (Bitcoin's "inbound optional" model):
//!
//! * **backbone** — proved inbound reachability (an inbound peer, an observed
//!   address, a STUN mapping, or a TCP hairpin probe). Advertises a dialable
//!   multiaddr and is eligible for seed listing.
//! * **gateway** — the one backbone-capable machine per LAN/egress, named by
//!   `GPN_SITE_GATEWAY=1` (or elected by witnessed uptime when
//!   `GPN_SITE_ELECT=1`). It is a backbone node with the extra job of being the
//!   dial target for the other machines on that site.
//! * **leaf** — everything else. It syncs, mines and serves jobs, but publishes
//!   **no dialable multiaddr** and claims no seed eligibility.
//!
//! # Behaviour (v2.3 — dialable is the goal, never a gate)
//!
//! A node that can prove reachability is a **backbone node**: it announces its
//! endpoint and accepts inbound peers. Inbound reachability is what makes it a
//! seed/backbone member, but it is **never a gate on joining** — dialability
//! cannot be a requirement when most home connections sit behind NAT or CGNAT
//! and jobs are *pulled* by the node anyway. A node that cannot be dialled runs
//! as a **leaf**, prints the operator notice once (with the exact port-forward
//! fix) and **keeps syncing, mining and serving**.
//!
//! * Probe is ON by default. `GPN_REQUIRE_DIALABLE=0` (also `false`/`no`/`off`)
//!   skips the probe entirely (local/single-node testing).
//! * Soft mode is the **default**: the node logs the actionable
//!   [`degradation_notice`] block once, continues as a non-discoverable
//!   participant, re-checks every 10 minutes and prints [`promotion_notice`]
//!   once when it becomes dialable.
//! * `--require-backbone` / `GPN_REQUIRE_BACKBONE=1` restores the exit-
//!   [`EXIT_NOT_DIALABLE`] (78) hard gate for operators who must guarantee
//!   reachability. It is opt-in and deprecated for general use.
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
use std::net::{IpAddr, Ipv6Addr};
use std::time::{Duration, Instant};

/// Distinct process exit code used when the node is not dialable.
///
/// 78 is `EX_CONFIG` (sysexits.h): "configuration error". The `gpn_node`
/// supervisor maps it to `down(not dialable)` in the status line instead of a
/// generic crash.
pub const EXIT_NOT_DIALABLE: i32 = 78;

/// The reachability tier a node may claim, derived from evidence only.
///
/// Never self-assigned: the startup gate (and the 10-minute re-check) produce
/// it from an inbound peer / observed address / STUN / TCP probe, and the
/// access point stores what the signed heartbeat reports. The `gateway` flag is
/// orthogonal metadata for the one dialable machine per site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reachability {
    /// Proved inbound reachability; may publish a dialable multiaddr.
    Backbone,
    /// Not provably reachable; participates but publishes no dialable address.
    Leaf,
}

impl Reachability {
    /// Wire value used by the provider heartbeat and `/providers`.
    pub fn as_str(self) -> &'static str {
        match self {
            Reachability::Backbone => "backbone",
            Reachability::Leaf => "leaf",
        }
    }
}

/// The tier for a verdict. A dialable node is backbone; everything else is a
/// leaf (SPEC 47: "exit 78 on a home router" is why stock installs died).
pub fn reachability_of(dialable: bool) -> Reachability {
    if dialable {
        Reachability::Backbone
    } else {
        Reachability::Leaf
    }
}

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
            } else if v6.to_ipv4_mapped().is_some() {
                // `::ffff:a.b.c.d` is an IPv4 endpoint wearing a v6 costume.
                // Not a native v6 address: a peer can never dial it as v6.
                IpClass::Private
            } else if is_global_unicast_v6(v6) {
                IpClass::Public
            } else {
                // Reserved, NAT64 (64:ff9b::/96), discard-only (100::/64) and
                // documentation (2001:db8::/32) space are not reachable.
                IpClass::Private
            }
        }
    }
}

/// IPv6 global unicast is `2000::/3`.
pub fn is_global_unicast_v6(ip: &Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    (first & 0xe000) == 0x2000 && !is_documentation_v6(ip)
}

/// `2001:db8::/32` — documentation space, never a live node address.
pub fn is_documentation_v6(ip: &Ipv6Addr) -> bool {
    ip.segments()[0] == 0x2001 && ip.segments()[1] == 0x0db8
}

/// The /64 prefix of an address, used to detect ISP renumbering.
pub fn prefix64(ip: &Ipv6Addr) -> [u8; 8] {
    let o = ip.octets();
    let mut p = [0u8; 8];
    p.copy_from_slice(&o[..8]);
    p
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

/// Count peers that dialed us **over IPv6** (the local side of the inbound
/// connection is a global v6 address). This is v6-specific evidence: a v4
/// inbound peer must not make a v6 address publishable.
pub fn inbound_v6_peer_count(state: &NetworkState) -> usize {
    state
        .connected_peers
        .values()
        .filter(|p| match &p.endpoint {
            // `local_addr` is the `multiaddr` crate's type; `endpoint_of`
            // speaks the sc-network type. Round-trip through the text form (a
            // peer count runs at most every poll, so this is not hot).
            PeerEndpoint::Listening { local_addr, .. } => local_addr
                .to_string()
                .parse::<Multiaddr>()
                .ok()
                .and_then(|ma| endpoint_of(&ma))
                .map(|(ip, _)| match ip {
                    IpAddr::V6(v6) => is_global_unicast_v6(&v6),
                    IpAddr::V4(_) => false,
                })
                .unwrap_or(false),
            _ => false,
        })
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
    /// Peers that dialed us **over IPv6** (see [`inbound_v6_peer_count`]).
    ///
    /// A v4 inbound peer must not satisfy a v6 configured endpoint: that would
    /// be a weaker path than the one v4 gets (design §5).
    pub inbound_v6_peers: usize,
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
            inbound_v6_peers: 0,
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
    evaluate_with(&GateInputs::new(
        inbound_peers,
        listen,
        observed,
        configured,
    ))
}

/// The pure gate decision, including DNS resolution and active probes.
pub fn evaluate_with(inputs: &GateInputs) -> Verdict {
    let GateInputs {
        inbound_peers,
        inbound_v6_peers,
        listen,
        observed,
        configured,
        resolved,
        stun_mapped,
        tcp_probe_ok,
    } = inputs;
    let (inbound_peers, inbound_v6_peers, listen, observed, resolved, stun_mapped, tcp_probe_ok) = (
        *inbound_peers,
        *inbound_v6_peers,
        *listen,
        *observed,
        *resolved,
        *stun_mapped,
        *tcp_probe_ok,
    );
    let configured = *configured;

    // Inbound evidence must come from the same family as the endpoint it is
    // meant to prove. For a v6 configured endpoint only a peer that actually
    // connected over v6 counts; otherwise a v4 dialer would make an unreachable
    // v6 address look dialable (design §5: "a v4 pass alone does not make a v6
    // address publishable").
    let requires_v6 = configured.map(is_v6_endpoint).unwrap_or(false);
    let (inbound_ok, inbound_desc) = if requires_v6 {
        (inbound_v6_peers, "over IPv6")
    } else {
        (inbound_peers, "")
    };
    if inbound_ok > 0 {
        return Verdict::pass(
            Evidence::InboundPeer,
            format!("{inbound_ok} peer(s) dialed this node (inbound connection{inbound_desc})"),
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
        IpClass::Private => "private/ULA/non-global (RFC1918, CGNAT, fc00::/7, v4-mapped)",
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

/// External STUN binding on the given address family: what endpoint does the
/// network map our socket to?
///
/// Genuinely external evidence (a third party sees our mapped tuple), unlike a
/// hairpin connect. Returns `None` on any error/timeout (including no route
/// for that family).
pub async fn stun_probe_family(
    server: &str,
    want_v6: bool,
    timeout: Duration,
) -> Option<(IpAddr, u16)> {
    use std::net::ToSocketAddrs;
    let target = server
        .to_socket_addrs()
        .ok()?
        .find(|sa| sa.is_ipv6() == want_v6)?;
    let bind = if want_v6 { "[::]:0" } else { "0.0.0.0:0" };
    let sock = tokio::net::UdpSocket::bind(bind).await.ok()?;
    sock.connect(target).await.ok()?;
    sock.send(&build_stun_binding_request()).await.ok()?;
    let mut buf = [0u8; 512];
    let n = tokio::time::timeout(timeout, sock.recv(&mut buf))
        .await
        .ok()?
        .ok()?;
    parse_stun_xor_mapped(&buf[..n])
}

/// External STUN binding, trying IPv4 first and falling back to IPv6.
///
/// Kept for the v4-first path; callers that know the configured endpoint's
/// family use [`stun_probe_family`] directly.
pub async fn stun_probe(server: &str, timeout: Duration) -> Option<(IpAddr, u16)> {
    match stun_probe_family(server, false, timeout).await {
        Some(v) => Some(v),
        None => stun_probe_family(server, true, timeout).await,
    }
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

    // IPv6 endpoints must be probed over IPv6: an IPv4 STUN answer can never
    // confirm a v6 mapping (and a v6-only host has no v4 route at all).
    let want_v6 = resolved.iter().all(|ip| ip.is_ipv6());

    if let Some(server) = stun_server {
        if let Some((ip, mapped_port)) =
            stun_probe_family(server, want_v6, Duration::from_secs(STUN_TIMEOUT_SECS)).await
        {
            if mapped_port == port && resolved.contains(&ip) {
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
            SafetyReason::SoloChain => {
                "this node is the only node on the network (0 peers connected)"
            }
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

/// Decide whether the hard gate must stop the process.
///
/// SPEC 47: only an operator-requested hard gate (`--require-backbone` /
/// `GPN_REQUIRE_BACKBONE=1`) may exit, and never after the sole-authority
/// safety valve fired. Soft mode (the default) always keeps running as a leaf.
pub fn hard_gate_must_exit(hard_gate: bool, safety_override: bool) -> bool {
    hard_gate && !safety_override
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
    !matches!(
        raw.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        Some("0" | "false" | "no" | "off")
    )
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
    /// Hard gate (`--require-backbone`, **opt-in**): exit
    /// [`EXIT_NOT_DIALABLE`] when the probe fails. The default is soft mode —
    /// warn once, run degraded, re-check every 10 minutes.
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
    /// `GPN_SITE_GATEWAY=1`: this machine is the dialable gateway for its
    /// LAN/egress. It always attempts UPnP/manual forwarding and advertises.
    pub site_gateway: bool,
    /// `GPN_SITE_ELECT=1`: allow the witnessed-uptime fallback to promote one
    /// leaf to gateway automatically. Default off — election needs data the
    /// peers already publish, and never a new consensus field.
    pub site_elect: bool,
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

        let site_gateway = parse_site_gateway(std::env::var("GPN_SITE_GATEWAY").ok().as_deref());
        // A site gateway is by definition dialable-capable: forcing the role
        // upgrades it even if the operator also passed --no-discoverable.
        let discoverable =
            site_gateway || parse_discoverable(std::env::var("GPN_DISCOVERABLE").ok().as_deref());
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
            site_gateway,
            site_elect: parse_site_gateway(std::env::var("GPN_SITE_ELECT").ok().as_deref()),
        }
    }

    /// Explicit constructor (used by tests / callers with resolved values).
    pub fn new(require: bool, timeout: Duration, public_addr: Option<Multiaddr>) -> Self {
        GateConfig {
            require,
            require_backbone: false,
            seed: true,
            discoverable: true,
            timeout,
            public_addr,
            stun_server: None,
            safety: true,
            site_gateway: false,
            site_elect: false,
        }
    }

    /// The capability tier this configuration aims for (for logs/status).
    ///
    /// This is the *intent*; the honoured tier comes from peer witnesses
    /// (`gpn-liveness`), never from a self-reported flag.
    pub fn intended_tier(&self, dialable: bool) -> &'static str {
        if !self.discoverable {
            "private"
        } else if self.site_gateway {
            if dialable {
                "gateway"
            } else {
                "gateway (unreachable)"
            }
        } else if dialable {
            if self.seed {
                "seed-candidate"
            } else {
                "backbone"
            }
        } else {
            "leaf"
        }
    }
}

/// Parse the `GPN_REQUIRE_DIALABLE` value. Default **ON**; only an explicit
/// false-y value opts out.
pub fn parse_require_dialable(raw: Option<&str>) -> bool {
    !matches!(
        raw.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        Some("0" | "false" | "no" | "off")
    )
}

/// Parse `GPN_SEED`. Default **ON**; an explicit false-y value opts out of
/// seed publication (and silences the degradation notice).
pub fn parse_seed(raw: Option<&str>) -> bool {
    !matches!(
        raw.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        Some("0" | "false" | "no" | "off")
    )
}

/// Parse `GPN_DISCOVERABLE`. Default **ON**.
pub fn parse_discoverable(raw: Option<&str>) -> bool {
    !matches!(
        raw.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        Some("0" | "false" | "no" | "off")
    )
}

/// Parse `GPN_REQUIRE_BACKBONE`. Default **OFF** (soft mode) — a node that
/// cannot be dialled prints the §5.2 notice once and keeps serving as a leaf.
///
/// Only an explicit truth-y value (`1`/`true`/`yes`/`on`) turns the hard gate
/// back on, in which case a non-dialable node exits [`EXIT_NOT_DIALABLE`].
/// Anything else — unset, empty, or a false-y value — means soft mode.
pub fn parse_require_backbone(raw: Option<&str>) -> bool {
    matches!(
        raw.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

/// Parse `GPN_SITE_GATEWAY` / `GPN_SITE_ELECT`. Default **off**; only an
/// explicit truth-y value enables the role (SPEC 47).
pub fn parse_site_gateway(raw: Option<&str>) -> bool {
    matches!(
        raw.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

// ── Port-mapping outcome (for the degradation notice) ────────────────────

/// What the automatic port-mapping ladder said, rendered into the operator
/// notice so "the port is not open" always carries a *why*.
///
/// Kept independent of [`crate::portmap::MapOutcome`] so the notice stays a
/// pure, testable function; the service maps one to the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortMapHint {
    /// A rule exists: `external` is what the internet sees, `lease_secs` what
    /// the router promised.
    Granted { external: String, lease_secs: u64 },
    /// The router answered and said no (disabled UPnP, no IGD, refused).
    Refused(String),
    /// `GPN_UPNP=off` / `--no-upnp`: nothing was sent.
    Disabled,
    /// An explicit public address is configured, so mapping was skipped.
    Skipped,
    /// The WAN address is CGNAT/private: no forward can ever work.
    Cgnat(String),
    /// No UPnP attempt was made at all (probe disabled/private mode).
    Unknown,
}

impl PortMapHint {
    /// One line, log-safe, ASCII, shown inside the degradation block.
    pub fn line(&self) -> String {
        match self {
            PortMapHint::Granted {
                external,
                lease_secs,
            } => format!(
                "portmap: granted -> {external} (lease {} min) - the router DID open the port; if\n     the node is still not dialable, the ISPs/upstream firewall is blocking it",
                lease_secs / 60
            ),
            PortMapHint::Refused(reason) => {
                format!("portmap: refused by router ({reason})")
            }
            PortMapHint::Disabled => "portmap: disabled (GPN_UPNP=off)".to_string(),
            PortMapHint::Skipped => {
                "portmap: skipped (public address configured)".to_string()
            }
            PortMapHint::Cgnat(wan) => format!(
                "portmap: refused by router (CGNAT WAN address {wan}: no port forward can work)"
            ),
            PortMapHint::Unknown => "portmap: not attempted".to_string(),
        }
    }
}

// ── Site election: witnessed uptime ──────────────────────────────────────

/// What peers observed about one candidate: the window it was *witnessed*
/// online, and how many independent peers saw it. Self-reported uptime is
/// deliberately absent — a node cannot vote for itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UptimeSample {
    pub peer_id: String,
    /// Unix seconds of the first peer observation in the window.
    pub first_witnessed_secs: u64,
    /// Unix seconds of the most recent peer observation.
    pub last_witnessed_secs: u64,
    /// Distinct peers that reported seeing it (capped when scoring).
    pub witnesses: u32,
}

impl UptimeSample {
    /// Seconds this peer was continuously witnessed (never negative).
    pub fn witnessed_secs(&self) -> u64 {
        self.last_witnessed_secs
            .saturating_sub(self.first_witnessed_secs)
    }

    /// Score used for election: witnessed duration, weighted by independent
    /// witnesses (capped at 4 so one peer cannot dominate by many sessions).
    pub fn score(&self) -> u64 {
        self.witnessed_secs()
            .saturating_mul(u64::from(self.witnesses.min(4)))
    }
}

/// Pick the site gateway from peer-observed uptime.
///
/// Highest [`UptimeSample::score`] wins; ties break on the lexicographically
/// smallest peer id so every node computes the same answer without talking to
/// anyone. Returns `None` for an empty candidate set.
///
/// Uptime never decides **ownership** of a socket — a handshake does
/// ([`judge_claim`]). It only ranks candidates that already hold a *verified*
/// claim on **different** sockets behind one public IP (the "one dial target
/// per LAN" choice). Wired through [`gateway_role`].
pub fn elect_gateway(candidates: &[UptimeSample]) -> Option<&UptimeSample> {
    candidates.iter().min_by(|a, b| {
        b.score()
            .cmp(&a.score())
            .then_with(|| a.peer_id.cmp(&b.peer_id))
    })
}

/// Should this node make itself a gateway because an election ran?
///
/// Only when the operator opted in (`GPN_SITE_ELECT=1`) AND this peer id wins
/// the witnessed-uptime comparison. The public function is separate from the
/// service so the rule can be tested without a network.
pub fn elected_as_gateway(
    cfg: &GateConfig,
    self_peer_id: &str,
    candidates: &[UptimeSample],
) -> bool {
    cfg.site_elect && elect_gateway(candidates).map(|s| s.peer_id.as_str()) == Some(self_peer_id)
}

// ── Public-socket ownership: one verified gateway per public IP ──────────
//
// The governing rule (design-lan-topology §3.0):
//
//   An address may be published only by the peer id that a third party
//   reaches when it dials that address.
//
// Election is therefore **not a vote**: it is the outcome of a dial-back. A
// bare TCP connect is not evidence (two machines behind one NAT both "prove"
// the same socket that way); only a libp2p handshake that returns the expected
// peer id is.

/// What a dial-back got back from a socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialbackOutcome {
    /// A handshake completed and the remote peer id is the expected one.
    Verified { peer_id: String },
    /// A handshake completed, but somebody **else** answered. This is box B's
    /// case today: the router forwards the socket to another machine.
    WrongPeer { expected: String, actual: String },
    /// No handshake (timeout, refused, protocol error).
    Unreachable { reason: String },
    /// Not attempted because the verifier shares the claimant's egress.
    /// Never a pass: verification from inside the same NAT is what lied.
    Skipped { reason: String },
}

impl DialbackOutcome {
    /// Classify a dial-back result that was performed from a *third-party*
    /// vantage (the egress check is [`judge_claim`]'s job).
    pub fn classify(observed_peer_id: Option<&str>, expected: &str) -> Self {
        match observed_peer_id {
            None => DialbackOutcome::Unreachable {
                reason: "no handshake".to_string(),
            },
            Some(actual) if actual == expected => DialbackOutcome::Verified {
                peer_id: actual.to_string(),
            },
            Some(actual) => DialbackOutcome::WrongPeer {
                expected: expected.to_string(),
                actual: actual.to_string(),
            },
        }
    }

    /// Only [`DialbackOutcome::Verified`] may make an address publishable.
    pub fn is_verified(&self) -> bool {
        matches!(self, DialbackOutcome::Verified { .. })
    }

    /// Stable wire/diagnostic code.
    pub fn code(&self) -> &'static str {
        match self {
            DialbackOutcome::Verified { .. } => "verified",
            DialbackOutcome::WrongPeer { .. } => "wrong-peer",
            DialbackOutcome::Unreachable { .. } => "unreachable",
            DialbackOutcome::Skipped { .. } => "skipped",
        }
    }

    /// Human-readable reason. The `WrongPeer` text names the id that actually
    /// answered — box B's symptom must be diagnosable from one log line.
    pub fn summary(&self) -> String {
        match self {
            DialbackOutcome::Verified { peer_id } => {
                format!("socket answers as {peer_id}")
            }
            DialbackOutcome::WrongPeer { expected, actual } => format!(
                "socket answers as {actual}, not {expected} — another machine owns this forward"
            ),
            DialbackOutcome::Unreachable { reason } => {
                format!("no libp2p handshake from the public socket ({reason})")
            }
            DialbackOutcome::Skipped { reason } => {
                format!("verification skipped ({reason})")
            }
        }
    }
}

/// One claim that a public socket answers as `peer_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicClaim {
    /// The socket: `ip:port`, no `/p2p/<id>` suffix.
    pub socket: String,
    pub peer_id: String,
    /// Unix seconds when a verifier saw the handshake.
    pub verified_at: u64,
    /// The verifier's own peer id.
    pub verifier_id: String,
    /// The verifier's public IP. Equal to the claim's IP ⇒ same egress.
    pub verifier_egress: String,
    /// Unix seconds after which the claim must be re-verified.
    pub expires_at: u64,
}

impl PublicClaim {
    pub fn new(
        socket: impl Into<String>,
        peer_id: impl Into<String>,
        verifier_id: impl Into<String>,
        verifier_egress: impl Into<String>,
        verified_at: u64,
        ttl_secs: u64,
    ) -> Self {
        PublicClaim {
            socket: socket.into(),
            peer_id: peer_id.into(),
            verified_at,
            verifier_id: verifier_id.into(),
            verifier_egress: verifier_egress.into(),
            expires_at: verified_at.saturating_add(ttl_secs),
        }
    }

    pub fn is_live(&self, now: u64) -> bool {
        now < self.expires_at
    }

    /// `ip` part of the socket (`"1.2.3.4"` from `"1.2.3.4:30333"`).
    pub fn socket_ip(&self) -> Option<&str> {
        match self.socket.rsplit_once(':') {
            Some((ip, _)) if !ip.is_empty() => Some(ip),
            _ => None,
        }
    }
}

/// Who is verifying, from where, and what the handshake returned.
#[derive(Debug, Clone, Copy)]
pub struct VerifierView<'a> {
    pub verifier_id: &'a str,
    /// The verifier's own public IP as an observer on the internet sees it.
    pub verifier_egress: &'a str,
    /// The peer id the handshake returned (`None` = no handshake).
    pub observed_peer_id: Option<&'a str>,
}

/// Decide one claim from a verifier's view.
///
/// Same egress is **never** a pass: if the verifier shares the claimant's
/// public IP, the check is skipped and the claim stays unverified. This is the
/// one rule that would have prevented the 2026-11-12 incident even with the
/// old code.
pub fn judge_claim(claim: &PublicClaim, view: &VerifierView<'_>) -> DialbackOutcome {
    if claim.socket_ip() == Some(view.verifier_egress) {
        return DialbackOutcome::Skipped {
            reason: "same-egress".to_string(),
        };
    }
    DialbackOutcome::classify(view.observed_peer_id, &claim.peer_id)
}

/// What the registry did with an offered claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimDecision {
    /// The claim now owns its socket.
    Accepted,
    /// The claim displaced a **different** peer id on the same socket. The old
    /// owner must demote itself on its next cycle (it will, from its own
    /// re-verification returning `WrongPeer`) — no restart, no operator.
    Superseded { previous: String },
    /// Refused: the claim is expired, or not verified.
    Rejected { reason: String },
}

/// Socket-ownership registry: at most one live verified claim per socket, and
/// the newest verified claim wins.
#[derive(Debug, Default, Clone)]
pub struct SocketRegistry {
    claims: std::collections::BTreeMap<String, PublicClaim>,
}

impl SocketRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Offer a claim. Only live, verifier-passing claims are accepted; a newer
    /// verified claim on the same socket displaces the older one.
    ///
    /// `verified` is the outcome the *caller* obtained. A claim that is not
    /// `Verified` is rejected here, so a caller cannot smuggle one in.
    pub fn offer(
        &mut self,
        claim: PublicClaim,
        verified: &DialbackOutcome,
        now: u64,
    ) -> ClaimDecision {
        if !claim.is_live(now) {
            return ClaimDecision::Rejected {
                reason: "expired".to_string(),
            };
        }
        if !verified.is_verified() {
            return ClaimDecision::Rejected {
                reason: verified.code().to_string(),
            };
        }
        match self.claims.get(&claim.socket) {
            Some(existing) if existing.peer_id != claim.peer_id && existing.is_live(now) => {
                let previous = existing.peer_id.clone();
                self.claims.insert(claim.socket.clone(), claim);
                ClaimDecision::Superseded { previous }
            }
            _ => {
                self.claims.insert(claim.socket.clone(), claim);
                ClaimDecision::Accepted
            }
        }
    }

    /// The live owner of `socket`, if any.
    pub fn owner(&self, socket: &str, now: u64) -> Option<&PublicClaim> {
        self.claims.get(socket).filter(|c| c.is_live(now))
    }

    /// Does `peer_id` hold a live verified claim on `socket`?
    pub fn holds(&self, socket: &str, peer_id: &str, now: u64) -> bool {
        self.owner(socket, now)
            .map(|c| c.peer_id == peer_id)
            .unwrap_or(false)
    }

    /// Drop a claim (an explicit demotion, e.g. our own re-check got WrongPeer).
    pub fn demote(&mut self, socket: &str, peer_id: &str) {
        if self
            .claims
            .get(socket)
            .map(|c| c.peer_id == peer_id)
            .unwrap_or(false)
        {
            self.claims.remove(socket);
        }
    }

    /// Live claims (used by the verifier loop and by tests).
    pub fn live_claims(&self, now: u64) -> Vec<&PublicClaim> {
        self.claims.values().filter(|c| c.is_live(now)).collect()
    }
}

/// The tier this node may hold, derived from verified socket ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayRole {
    /// Holds the site's dial target: verified claim + elected among the
    /// verified claimants behind this public IP.
    Gateway,
    /// Verified on its own socket, but not the site's dial target.
    Backbone,
    /// No verified claim: publishes nothing.
    Leaf,
}

impl GatewayRole {
    pub fn is_gateway(self) -> bool {
        matches!(self, GatewayRole::Gateway)
    }
    pub fn is_dialable(self) -> bool {
        !matches!(self, GatewayRole::Leaf)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            GatewayRole::Gateway => "gateway",
            GatewayRole::Backbone => "backbone",
            GatewayRole::Leaf => "leaf",
        }
    }
}

/// Compute this node's role.
///
/// * `dialable` — the gate's own verdict (evidence that somebody reached us).
/// * `socket` — our public socket (`ip:port`) as a string, if we have one.
/// * `claims` — claims from the verifier (see [`read_claims_file`]).
/// * `samples` — witnessed uptime for the *verified claimants*; used only to
///   rank several verified sockets behind one public IP.
///
/// With election off (`GPN_SITE_ELECT` unset) this is exactly today's rule:
/// `GPN_SITE_GATEWAY=1` names the gateway, everything else dialable is
/// backbone. With election on, a node needs a live verified claim on its own
/// socket, and exactly one claimant per public IP wins.
pub fn gateway_role(
    cfg: &GateConfig,
    self_peer_id: &str,
    socket: Option<&str>,
    claims: &[PublicClaim],
    samples: &[UptimeSample],
    now: u64,
) -> GatewayRole {
    let mut reg = SocketRegistry::new();
    for c in claims {
        // Claims that reached us are already verified by the verifier; the
        // registry re-checks liveness and the socket key.
        let outcome = DialbackOutcome::Verified {
            peer_id: c.peer_id.clone(),
        };
        let _ = reg.offer(c.clone(), &outcome, now);
    }

    if !cfg.site_elect {
        // Explicit naming (today's behaviour): the operator says which machine
        // is the gateway; everything else is unchanged.
        return if cfg.site_gateway {
            GatewayRole::Gateway
        } else if cfg.discoverable {
            GatewayRole::Backbone
        } else {
            GatewayRole::Leaf
        };
    }

    // Election: verification decides, uptime only ranks.
    let Some(socket) = socket else {
        return GatewayRole::Leaf;
    };
    if !reg.holds(socket, self_peer_id, now) {
        // No live verified claim for us on our own socket: not publishable.
        return GatewayRole::Leaf;
    }
    // Who else holds a verified claim behind this public IP?
    let our_ip = claims
        .iter()
        .find(|c| c.socket == socket && c.peer_id == self_peer_id)
        .and_then(|c| c.socket_ip())
        .map(|s| s.to_string());
    let mut peers: Vec<String> = reg
        .live_claims(now)
        .into_iter()
        .filter(|c| c.socket_ip() == our_ip.as_deref())
        .map(|c| c.peer_id.clone())
        .collect();
    peers.sort();
    peers.dedup();
    if peers.len() <= 1 {
        return GatewayRole::Gateway;
    }
    // Several verified sockets behind one IP: rank by witnessed uptime.
    let ranked: Vec<UptimeSample> = peers
        .iter()
        .map(|pid| {
            samples
                .iter()
                .find(|s| &s.peer_id == pid)
                .cloned()
                .unwrap_or(UptimeSample {
                    peer_id: pid.clone(),
                    first_witnessed_secs: now,
                    last_witnessed_secs: now,
                    witnesses: 0,
                })
        })
        .collect();
    if elected_as_gateway(cfg, self_peer_id, &ranked) {
        GatewayRole::Gateway
    } else {
        GatewayRole::Backbone
    }
}

/// Read verified claims published by the verifier (the AP or a witness).
///
/// Contract: a JSON array of
/// `{socket, peer_id, verified_at, verifier_id, verifier_egress, expires_at}`.
/// A missing or unreadable file yields no claims — which means, with election
/// on, Leaf. Never a guess.
pub fn read_claims_file(path: &std::path::Path) -> Vec<PublicClaim> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|e| {
            Some(PublicClaim {
                socket: e.get("socket")?.as_str()?.trim().to_string(),
                peer_id: e.get("peer_id")?.as_str()?.trim().to_string(),
                verified_at: e.get("verified_at")?.as_u64()?,
                verifier_id: e
                    .get("verifier_id")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string(),
                verifier_egress: e
                    .get("verifier_egress")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string(),
                expires_at: e.get("expires_at")?.as_u64()?,
            })
        })
        .filter(|c| !c.socket.is_empty() && !c.peer_id.is_empty())
        .collect()
}

/// Where the verifier's claims live: `$GPN_CLAIMS_STATE`, else
/// `$HOME/.gpn/claims.json`.
pub fn claims_state_path() -> std::path::PathBuf {
    use std::path::PathBuf;
    if let Ok(p) = std::env::var("GPN_CLAIMS_STATE") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".gpn").join("claims.json")
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

// ── IPv6 detection & publication ─────────────────────────────────────────

/// One IPv6 address found on a local interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfaceV6 {
    /// Interface name (`eth0`, `en0`, `Ethernet 2`, …).
    pub iface: String,
    /// The address itself.
    pub addr: Ipv6Addr,
    /// RFC 4941 temporary/privacy address (`IFA_F_TEMPORARY`).
    pub temporary: bool,
}

/// A selected candidate for the public v6 endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V6Candidate {
    pub iface: String,
    pub addr: Ipv6Addr,
    pub temporary: bool,
    /// The kernel picks this address as the source for a global v6 route.
    pub default_route: bool,
}

/// Result of [`select_public_v6`]: at most one candidate per interface,
/// ordered best-first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct V6Selection {
    pub candidates: Vec<V6Candidate>,
    /// A temporary address had to be used (no stable address on that iface).
    pub used_temporary: bool,
    /// Interfaces where only a temporary address existed.
    pub temporary_only_ifaces: Vec<String>,
}

impl V6Selection {
    /// The address we publish (default-route interface, stable first).
    pub fn best(&self) -> Option<&V6Candidate> {
        self.candidates.first()
    }

    pub fn addr(&self) -> Option<Ipv6Addr> {
        self.best().map(|c| c.addr)
    }

    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }
}

/// Pure address selection: one candidate per interface, stable preferred over
/// temporary, the default-route interface first.
///
/// `default_src` is the source address the kernel chooses for a route to a
/// global v6 destination (see [`default_route_source_v6`]); it is used only as
/// a tie-break and may be `None` (no v6 route).
pub fn select_public_v6(ifaces: &[IfaceV6], default_src: Option<Ipv6Addr>) -> V6Selection {
    use std::collections::BTreeMap;

    // Group by interface. The kernel's chosen default-route source wins on its
    // own interface; otherwise keep the lexicographically smallest stable
    // address, falling back to the smallest temporary one.
    let mut stable: BTreeMap<&str, Ipv6Addr> = BTreeMap::new();
    let mut temporary: BTreeMap<&str, Ipv6Addr> = BTreeMap::new();
    for a in ifaces {
        if !is_global_unicast_v6(&a.addr) {
            continue;
        }
        let slot = if a.temporary {
            &mut temporary
        } else {
            &mut stable
        };
        slot.entry(a.iface.as_str())
            .and_modify(|cur| {
                // A default-route address is the best possible choice on this
                // interface; never let a smaller address displace it.
                if Some(*cur) == default_src {
                    return;
                }
                if Some(a.addr) == default_src || a.addr < *cur {
                    *cur = a.addr;
                }
            })
            .or_insert(a.addr);
    }

    let mut candidates: Vec<V6Candidate> = Vec::new();
    let mut temporary_only_ifaces: Vec<String> = Vec::new();
    let iface_names: std::collections::BTreeSet<&str> =
        stable.keys().chain(temporary.keys()).copied().collect();
    for iface in iface_names {
        let (addr, temporary) = match (stable.get(iface), temporary.get(iface)) {
            (Some(a), _) => (*a, false),
            (None, Some(a)) => {
                temporary_only_ifaces.push(iface.to_string());
                (*a, true)
            }
            (None, None) => continue,
        };
        candidates.push(V6Candidate {
            iface: iface.to_string(),
            addr,
            temporary,
            default_route: default_src == Some(addr),
        });
    }

    // Best first: default-route interface, then stable before temporary, then
    // a deterministic name/address order.
    candidates.sort_by(|a, b| {
        (!a.default_route, a.temporary, &a.iface, a.addr.octets()).cmp(&(
            !b.default_route,
            b.temporary,
            &b.iface,
            b.addr.octets(),
        ))
    });

    let used_temporary = candidates.iter().any(|c| c.temporary);
    V6Selection {
        candidates,
        used_temporary,
        temporary_only_ifaces,
    }
}

/// Source address the kernel would use for a global v6 destination.
///
/// No packet is sent: a UDP `connect` only asks the routing table for a source
/// address. Returns `None` when the host has no IPv6 route at all, which is the
/// common case for v4-only machines.
pub fn default_route_source_v6() -> Option<Ipv6Addr> {
    let sock = std::net::UdpSocket::bind("[::]:0").ok()?;
    // Documentation space for the *destination*: never routed, but enough for
    // the kernel to pick the source address of the default v6 route.
    sock.connect("[2001:db8::1]:9").ok()?;
    match sock.local_addr().ok()?.ip() {
        IpAddr::V6(v6) if is_global_unicast_v6(&v6) => Some(v6),
        _ => None,
    }
}

/// Enumerate the host's IPv6 interface addresses.
///
/// On Linux the `getifaddrs` flags expose `IFA_F_TEMPORARY`; elsewhere the
/// address is treated as stable (privacy flags are not portable).
pub fn interface_v6_addrs() -> Vec<IfaceV6> {
    #[cfg(target_os = "linux")]
    {
        linux_interface_v6_addrs()
    }
    #[cfg(windows)]
    {
        windows_interface_v6_addrs()
    }
    #[cfg(all(not(target_os = "linux"), not(windows)))]
    {
        // No portable temporary-flag API: treat every address as stable.
        Vec::new()
    }
}

#[cfg(target_os = "linux")]
fn linux_interface_v6_addrs() -> Vec<IfaceV6> {
    use std::ffi::CStr;
    let mut out = Vec::new();
    // SAFETY: `getifaddrs`/`freeifaddrs` are the documented libc pair; every
    // pointer is checked for null before dereference and the list is freed
    // exactly once.
    unsafe {
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut head) != 0 {
            return out;
        }
        let mut cur = head;
        while !cur.is_null() {
            let ifa = &*cur;
            if !ifa.ifa_addr.is_null() && (*ifa.ifa_addr).sa_family as i32 == libc::AF_INET6 {
                let sin6 = &*(ifa.ifa_addr as *const libc::sockaddr_in6);
                let addr = Ipv6Addr::from(sin6.sin6_addr.s6_addr);
                let iface = CStr::from_ptr(ifa.ifa_name).to_string_lossy().into_owned();
                let temporary = (ifa.ifa_flags & libc::IFA_F_TEMPORARY as u32) != 0;
                out.push(IfaceV6 {
                    iface,
                    addr,
                    temporary,
                });
            }
            cur = ifa.ifa_next;
        }
        libc::freeifaddrs(head);
    }
    out
}

#[cfg(windows)]
fn windows_interface_v6_addrs() -> Vec<IfaceV6> {
    let mut out = Vec::new();
    let Ok(list) = if_addrs::get_if_addrs() else {
        return out;
    };
    for i in list {
        if let if_addrs::IfAddr::V6(v6) = i.addr {
            // getifaddrs on Windows does not expose privacy-address flags.
            out.push(IfaceV6 {
                iface: i.name,
                addr: v6.ip,
                temporary: false,
            });
        }
    }
    out
}

/// Detect the node's public IPv6 endpoint, if the host has one.
///
/// Combines [`interface_v6_addrs`] with [`default_route_source_v6`] and
/// [`select_public_v6`]. `None` means "no usable global unicast v6" — the
/// caller must then behave exactly as before (v4-only).
pub fn detect_public_ipv6() -> Option<V6Selection> {
    let ifaces = interface_v6_addrs();
    let sel = select_public_v6(&ifaces, default_route_source_v6());
    if sel.is_empty() {
        None
    } else {
        Some(sel)
    }
}

/// True when a multiaddress names an IPv6 endpoint (`/ip6` or `/dns6`).
pub fn is_v6_endpoint(ma: &Multiaddr) -> bool {
    ma.iter()
        .any(|p| matches!(p, Protocol::Ip6(_) | Protocol::Dns6(_)))
}

/// True when the address list already contains an IPv6 TCP listener.
pub fn has_v6_listener(addrs: &[Multiaddr]) -> bool {
    addrs.iter().any(|a| {
        let mut tcp = false;
        let mut v6 = false;
        for p in a.iter() {
            match p {
                Protocol::Ip6(_) => v6 = true,
                Protocol::Tcp(_) => tcp = true,
                _ => {}
            }
        }
        v6 && tcp
    })
}

/// Plan the listen set: keep every existing listener and, when a global v6
/// address exists, add `/ip6/::/tcp/<port>` (the wildcard survives an ISP
/// renumber; both libp2p-tcp and litep2p set `IPV6_V6ONLY` so it coexists with
/// `/ip4/0.0.0.0/tcp/<port>`).
pub fn plan_listen_addresses(
    existing: &[Multiaddr],
    have_global_v6: bool,
    port: u16,
) -> Vec<Multiaddr> {
    let mut out = existing.to_vec();
    if have_global_v6 && !has_v6_listener(existing) {
        if let Ok(ma) = format!("/ip6/::/tcp/{port}").parse::<Multiaddr>() {
            out.push(ma);
        }
    }
    out
}

/// The `/ip6/<addr>/tcp/<port>` base for publication, or `None` when the
/// address is not global unicast (never publish a non-dialable address).
pub fn v6_base(addr: &Ipv6Addr, port: u16) -> Option<Multiaddr> {
    if !is_global_unicast_v6(addr) {
        return None;
    }
    format!("/ip6/{addr}/tcp/{port}").parse().ok()
}

/// The full published base set: IPv4 first (unchanged behaviour), then the IPv6
/// entry when present. De-duplicated, order-preserving.
pub fn published_bases(v4: Option<&Multiaddr>, v6: Option<&Ipv6Addr>, port: u16) -> Vec<Multiaddr> {
    let mut out: Vec<Multiaddr> = Vec::new();
    if let Some(a) = v4 {
        out.push(a.clone());
    }
    if let Some(a) = v6.and_then(|a| v6_base(a, port)) {
        if !out.contains(&a) {
            out.push(a);
        }
    }
    out
}

/// The `Ipv6Addr` inside a `/ip6/...` multiaddress, if any.
pub fn ip6_of(ma: &Multiaddr) -> Option<Ipv6Addr> {
    ma.iter().find_map(|p| match p {
        Protocol::Ip6(v6) => Some(v6),
        _ => None,
    })
}

/// The first global-unicast v6 address peers reported for us on `port`.
/// This is the strongest v6-specific publication evidence that does not come
/// from the operator: a stranger actually saw us at that address.
pub fn observed_v6(observed: &[Multiaddr], port: u16) -> Option<Ipv6Addr> {
    observed.iter().find_map(|ma| {
        let (ip, p) = endpoint_of(ma)?;
        match ip {
            IpAddr::V6(v6) if p == port && is_global_unicast_v6(&v6) => Some(v6),
            _ => None,
        }
    })
}

/// What changed between two v6 detections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrefixChange {
    /// Neither before nor now.
    None,
    /// Identical address.
    Same,
    /// Same /64, different host part (privacy address churn).
    AddressChanged { old: Ipv6Addr, new: Ipv6Addr },
    /// The /64 prefix changed (ISP renumbering).
    PrefixRotated { old: Ipv6Addr, new: Ipv6Addr },
    /// v6 appeared.
    Appeared(Ipv6Addr),
    /// v6 disappeared.
    Disappeared(Ipv6Addr),
}

impl PrefixChange {
    /// True when the published endpoint must be replaced.
    pub fn needs_republish(&self) -> bool {
        !matches!(self, PrefixChange::None | PrefixChange::Same)
    }
}

/// Classify a change between the currently published v6 address and a fresh
/// detection.
pub fn prefix_change(current: Option<Ipv6Addr>, detected: Option<Ipv6Addr>) -> PrefixChange {
    match (current, detected) {
        (None, None) => PrefixChange::None,
        (Some(_), None) => PrefixChange::Disappeared(current.expect("checked")),
        (None, Some(new)) => PrefixChange::Appeared(new),
        (Some(old), Some(new)) => {
            if old == new {
                PrefixChange::Same
            } else if prefix64(&old) == prefix64(&new) {
                PrefixChange::AddressChanged { old, new }
            } else {
                PrefixChange::PrefixRotated { old, new }
            }
        }
    }
}

/// Republish action produced by [`rotation_action`].
pub struct RotationAction {
    pub change: PrefixChange,
    /// The new published base set (v4 preserved, stale v6 dropped).
    pub bases: Vec<Multiaddr>,
}

/// Decide whether a re-detection requires a republish, and compute the new
/// base set. `v4` is the unchanged IPv4 base; the old v6 address is dropped
/// whenever the detection differs.
pub fn rotation_action(
    v4: Option<&Multiaddr>,
    current_v6: Option<Ipv6Addr>,
    detected_v6: Option<Ipv6Addr>,
    port: u16,
) -> Option<RotationAction> {
    let change = prefix_change(current_v6, detected_v6);
    if !change.needs_republish() {
        return None;
    }
    let bases = published_bases(v4, detected_v6.as_ref(), port);
    Some(RotationAction { change, bases })
}

/// Re-detect the public IPv6 address every [`RECHECK_SECS`] and call
/// `republish` **once per change** with the new base set.
///
/// `evidence_ok` gates the republish: a new address that is not yet
/// evidence-backed must not be advertised (design §4). When it returns false
/// the change is logged and the previous address is kept.
pub async fn watch_ipv6_prefix<F, G>(
    v4: Option<Multiaddr>,
    initial_v6: Option<Ipv6Addr>,
    port: u16,
    evidence_ok: F,
    mut republish: G,
) where
    F: Fn(&Ipv6Addr) -> bool,
    G: FnMut(&RotationAction),
{
    let mut current = initial_v6;
    // Log the "changed but unproven" warning once per pending address, not on
    // every tick (design §6: log the change once).
    let mut pending_warned: Option<Ipv6Addr> = None;
    let mut tick = tokio::time::interval(Duration::from_secs(RECHECK_SECS));
    tick.tick().await; // consume the immediate first tick
    loop {
        tick.tick().await;
        let detected = detect_public_ipv6().and_then(|s| s.addr());
        let Some(action) = rotation_action(v4.as_ref(), current, detected, port) else {
            continue;
        };
        match detected {
            Some(new) if evidence_ok(&new) => {
                log::info!(
                    "dialability: IPv6 address changed ({:?}); republishing {} — stale address dropped",
                    action.change,
                    action
                        .bases
                        .iter()
                        .map(|b| b.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                current = Some(new);
                pending_warned = None;
                republish(&action);
            }
            Some(new) => {
                // Keep `current` at the still-published address so the next
                // tick re-tests this candidate; reverting to it would suppress
                // the republish when evidence finally arrives.
                if pending_warned != Some(new) {
                    log::warn!(
                        "dialability: IPv6 address changed to {new}, but it is not evidence-backed \
                         yet; keeping the previous endpoint until a peer observes it"
                    );
                    pending_warned = Some(new);
                }
            }
            None => {
                log::warn!(
                    "dialability: IPv6 address {} disappeared; dropping it from the published set",
                    current
                        .map(|a| a.to_string())
                        .unwrap_or_else(|| "(none)".into())
                );
                current = None;
                pending_warned = None;
                republish(&action);
            }
        }
    }
}

/// The operator-facing degradation notice (design §5.2), or `None` when the
/// operator opted out of seed publication.
///
/// `hard_gate` selects the postamble: soft mode (the **default**) keeps
/// running as a non-discoverable participant and promises the 10-minute
/// re-check; `--require-backbone` prints the notice and exits with
/// [`EXIT_NOT_DIALABLE`].
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
    portmap: &PortMapHint,
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
    let portmap_line = portmap.line();
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

  4. UPnP/NAT-PMP: the node tried this automatically (GPN_UPNP=auto is the
     default) - result:
       {portmap_line}
     The node keeps retrying every 10 minutes, so enabling UPnP on the router is
     picked up without a restart. Or use step 2. Disable with --no-upnp.

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

/// The endpoint set the node would publish if it is a backbone.
///
/// Written to the state file the provider heartbeat reads, on the startup
/// verdict and on every tier change in the re-check loop. A leaf writes no
/// multiaddrs, so a leaf can never be republished with a dialable address.
#[derive(Debug, Clone, Default)]
pub struct StatePublication {
    pub public: Option<Multiaddr>,
    pub public6: Option<Multiaddr>,
    pub source: String,
    /// The peer id this verdict belongs to.
    ///
    /// Written to the state file so a consumer can tell whose verdict it is. A
    /// node may only use a verdict that names **its own** peer id; a record
    /// without one (legacy) or naming another peer is Unknown and must be
    /// re-measured. Before this field existed the file held one unowned verdict
    /// and a short-lived probe's `leaf` was published as the live seed's own.
    pub peer_id: Option<String>,
    /// The identity of the process that produced the verdict (the probe).
    ///
    /// Normally the same as [`Self::peer_id`]; recorded separately because a
    /// verdict may in future be produced by a verifier that is not the node.
    pub probe_peer_id: Option<String>,
}

/// Path of the dialability state file: `$GPN_DIALABILITY_STATE`, else
/// `$HOME/.gpn/dialability.json`.
pub fn dialability_state_path() -> std::path::PathBuf {
    use std::path::PathBuf;
    if let Ok(p) = std::env::var("GPN_DIALABILITY_STATE") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".gpn").join("dialability.json")
}

/// Publish the verdict where `gpn-provider-status` and the heartbeat read it.
///
/// `dialable` is the single source of truth for address publication: a leaf
/// writes `multiaddrs: []` even if the caller passed endpoints (defence in
/// depth against a stale address being advertised after a tier downgrade).
pub fn write_state_file(
    publication: &StatePublication,
    dialable: bool,
    gateway: bool,
    detail: &str,
) {
    let path = dialability_state_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let multiaddrs: Vec<String> = if dialable {
        [publication.public.as_ref(), publication.public6.as_ref()]
            .into_iter()
            .flatten()
            .map(|a| a.to_string())
            .collect()
    } else {
        Vec::new()
    };
    let body = serde_json::json!({
        "public_endpoint": if dialable { publication.public.as_ref().map(|a| a.to_string()) } else { None },
        "public_endpoint_v6": if dialable { publication.public6.as_ref().map(|a| a.to_string()) } else { None },
        "multiaddrs": multiaddrs,
        "source": publication.source,
        // Identity key (2026-11-12): the verdict belongs to exactly this peer
        // id. A consumer that is not this peer must treat the record as
        // Unknown, never as its own.
        "peer_id": publication.peer_id,
        "probe_peer_id": publication
            .probe_peer_id
            .as_ref()
            .or(publication.peer_id.as_ref()),
        "dialable": dialable,
        "reachability": reachability_of(dialable).as_str(),
        "gateway": gateway,
        "detail": detail,
        "updated_unix": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    });
    let Ok(text) = serde_json::to_string_pretty(&body) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, text).is_err() || std::fs::rename(&tmp, &path).is_err() {
        log::debug!(
            "dialability: could not write state file {} (heartbeat will fall back to its own evidence)",
            path.display()
        );
    }
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
    publication: StatePublication,
) where
    N: NetworkStatusProvider + NetworkStateInfo + Send + Sync + 'static + ?Sized,
{
    watch_dialability_with_interval(
        network,
        cfg,
        initial_dialable,
        publication,
        Duration::from_secs(RECHECK_SECS),
    )
    .await
}

/// The re-check loop with an injectable interval (tests only).
///
/// SPEC 47 promotion lives here: the first tick that observes reachability on a
/// node that started as a leaf rewrites the state file to `backbone` and logs
/// exactly one line. The production interval is [`RECHECK_SECS`].
async fn watch_dialability_with_interval<N>(
    network: std::sync::Arc<N>,
    cfg: GateConfig,
    initial_dialable: bool,
    publication: StatePublication,
    interval: Duration,
) where
    N: NetworkStatusProvider + NetworkStateInfo + Send + Sync + 'static + ?Sized,
{
    if !cfg.require {
        return;
    }
    let mut last_dialable = initial_dialable;
    let mut tick = tokio::time::interval(interval);
    tick.tick().await; // consume the immediate first tick
    loop {
        tick.tick().await;
        let (inbound, inbound_v6) = match network.network_state().await {
            Ok(state) => (inbound_peer_count(&state), inbound_v6_peer_count(&state)),
            Err(_) => (0, 0),
        };
        let listen = network.listen_addresses();
        let observed = network.external_addresses();
        let verdict = full_verdict(
            inbound,
            inbound_v6,
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
                // Promote: rewrite the state file so the heartbeat starts
                // publishing the dialable endpoint (SPEC 47, step 2).
                write_state_file(&publication, true, cfg.site_gateway, &verdict.detail);
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
                // Demote: clear the published address immediately.
                write_state_file(&publication, false, cfg.site_gateway, &verdict.detail);
                log::warn!(
                    "dialability: lost inbound reachability ({})",
                    verdict.detail
                );
                if let Some(notice) = degradation_notice(
                    detect_lan_ipv4(),
                    port,
                    cfg.seed,
                    false,
                    &PortMapHint::Unknown,
                ) {
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
    inbound_v6: usize,
    listen: &[Multiaddr],
    observed: &[Multiaddr],
    configured: Option<&Multiaddr>,
    stun_server: Option<&str>,
) -> Verdict {
    // Passive evidence is always tried FIRST and for free. Only when it fails
    // do we pay for DNS + STUN + a TCP connect.
    let passive = evaluate_with(&GateInputs {
        inbound_peers: inbound,
        inbound_v6_peers: inbound_v6,
        listen,
        observed,
        configured,
        resolved: &[],
        stun_mapped: false,
        tcp_probe_ok: false,
    });
    if passive.dialable {
        return passive;
    }
    let Some(cfg_ma) = configured else {
        return passive;
    };
    let (resolved, stun_mapped, tcp_probe_ok) =
        probe_configured_endpoint(cfg_ma, stun_server).await;
    evaluate_with(&GateInputs {
        inbound_peers: inbound,
        inbound_v6_peers: inbound_v6,
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
        let (inbound, inbound_v6) = match network.network_state().await {
            Ok(state) => (inbound_peer_count(&state), inbound_v6_peer_count(&state)),
            Err(_) => (0, 0),
        };
        let listen = network.listen_addresses();
        let observed = network.external_addresses();

        // Passive evidence first — free, and it needs no network traffic.
        verdict = evaluate_with(&GateInputs {
            inbound_peers: inbound,
            inbound_v6_peers: inbound_v6,
            listen: &listen,
            observed: &observed,
            configured: cfg.public_addr.as_ref(),
            resolved: &[],
            stun_mapped: false,
            tcp_probe_ok: false,
        });
        if !verdict.dialable {
            if let Some(cfg_ma) = cfg.public_addr.as_ref() {
                let stale = cached_probe
                    .as_ref()
                    .map(|(at, _, _, _)| at.elapsed() >= Duration::from_secs(PROBE_REFRESH_SECS))
                    .unwrap_or(true);
                if stale {
                    let (resolved, stun_mapped, tcp_probe_ok) =
                        probe_configured_endpoint(cfg_ma, cfg.stun_server.as_deref()).await;
                    cached_probe = Some((Instant::now(), resolved, stun_mapped, tcp_probe_ok));
                }
                if let Some((_, resolved, stun_mapped, tcp_probe_ok)) = cached_probe.as_ref() {
                    verdict = evaluate_with(&GateInputs {
                        inbound_peers: inbound,
                        inbound_v6_peers: inbound_v6,
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

    /// Serialises tests that touch `GPN_DIALABILITY_STATE` (process-global).
    static STATE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
            host_port_of(&ma("/dns4/gpn-testnet.duckdns.org/tcp/30333/\
                 p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo")),
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
            inbound_v6_peers: 0,
            listen: &[ma("/ip4/0.0.0.0/tcp/30333")],
            observed: &observed,
            configured: Some(&cfg),
            resolved: &resolved,
            stun_mapped: false,
            tcp_probe_ok: false,
        });
        assert!(v.dialable, "detail: {}", v.detail);
        assert_eq!(v.evidence, Some(Evidence::ConfiguredAddrObserved));
        assert!(
            v.detail.contains("resolves to 142.129.54.88"),
            "{}",
            v.detail
        );
    }

    #[test]
    fn dns4_is_no_longer_rejected_as_unusable() {
        // The 2026-10-20 regression: a /dns4 GPN_PUBLIC_ADDR failed with
        // "not a usable /ip4|ip6/<addr>/tcp/<port> multiaddress".
        let cfg = ma("/dns4/gpn-testnet.duckdns.org/tcp/30333");
        let resolved: Vec<IpAddr> = vec!["142.129.54.88".parse().unwrap()];
        let v = evaluate_with(&GateInputs {
            inbound_peers: 0,
            inbound_v6_peers: 0,
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
            inbound_v6_peers: 0,
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
            inbound_v6_peers: 0,
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
            inbound_v6_peers: 0,
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
            inbound_v6_peers: 0,
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
        assert!(
            note.is_ascii(),
            "Windows consoles: notice must be ASCII only"
        );
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
    fn require_backbone_is_off_by_default() {
        // Soft mode unless the operator explicitly asks for the hard gate: an
        // unset/empty/garbage value must never stop a node from serving.
        assert!(!parse_require_backbone(None));
        assert!(!parse_require_backbone(Some("")));
        for v in ["0", "false", "FALSE", "no", "off", " 0 ", "banana"] {
            assert!(!parse_require_backbone(Some(v)), "{v} must stay soft");
        }
        // Explicit opt-in to the legacy hard gate.
        for v in ["1", "true", "TRUE", "yes", "on", " 1 "] {
            assert!(parse_require_backbone(Some(v)), "{v} must enable the gate");
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
        // SPEC 47: an unreachable discoverable node is a leaf now, not "degraded".
        assert_eq!(cfg.intended_tier(false), "leaf");
        cfg.seed = false;
        assert_eq!(cfg.intended_tier(true), "backbone");
    }

    #[test]
    fn degradation_notice_is_actionable_and_suppressed_by_no_seed() {
        let lan: IpAddr = "192.168.1.42".parse().unwrap();
        let note = degradation_notice(Some(lan), 30333, true, true, &PortMapHint::Unknown)
            .expect("notice when seeding");
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
        let generic = degradation_notice(None, 30333, true, true, &PortMapHint::Unknown).unwrap();
        assert!(generic.contains("LAN IP"), "{generic}");

        // The operator asked to be private: never nag.
        assert!(degradation_notice(Some(lan), 30333, false, true, &PortMapHint::Unknown).is_none());
        assert!(
            degradation_notice(Some(lan), 30333, false, false, &PortMapHint::Unknown).is_none()
        );
    }

    #[test]
    fn soft_mode_notice_promises_recheck_and_continues() {
        let lan: IpAddr = "192.168.1.42".parse().unwrap();
        let note = degradation_notice(Some(lan), 30333, true, false, &PortMapHint::Unknown)
            .expect("notice in soft mode");
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

    // ── IPv6: classification ──────────────────────────────────────────

    #[test]
    fn v6_classification_table() {
        let global = [
            "2606:4700:4700::1111",
            "2001:4860:4860::8888",
            "2a00:1450:4001:80b::200e",
            "3fff::1",
        ];
        for s in global {
            let ip: IpAddr = s.parse().unwrap();
            assert_eq!(classify_ip(&ip), IpClass::Public, "{s}");
            if let IpAddr::V6(v) = ip {
                assert!(is_global_unicast_v6(&v));
            }
        }
        let non_public = [
            ("fd00::1", IpClass::Private),          // ULA fc00::/7
            ("fc00::1", IpClass::Private),          // ULA
            ("fe80::1", IpClass::LinkLocal),        // link-local
            ("::1", IpClass::Loopback),             // loopback
            ("::", IpClass::Unspecified),           // unspecified
            ("::ffff:8.8.8.8", IpClass::Private),   // IPv4-mapped
            ("2001:db8::1", IpClass::Private),      // documentation
            ("64:ff9b::1.2.3.4", IpClass::Private), // NAT64
            ("100::1", IpClass::Private),           // discard-only
            ("ff02::1", IpClass::Multicast),        // multicast
        ];
        for (s, want) in non_public {
            let ip: IpAddr = s.parse().unwrap();
            assert_eq!(classify_ip(&ip), want, "{s}");
        }
    }

    fn iface(name: &str, addr: &str, temporary: bool) -> IfaceV6 {
        IfaceV6 {
            iface: name.into(),
            addr: addr.parse().unwrap(),
            temporary,
        }
    }

    #[test]
    fn selection_prefers_stable_over_temporary() {
        let ifaces = [
            iface("eth0", "2606:4700:1::10", true),
            iface("eth0", "2606:4700:1::20", false),
        ];
        let sel = select_public_v6(&ifaces, None);
        assert_eq!(sel.candidates.len(), 1);
        assert_eq!(sel.addr().unwrap().to_string(), "2606:4700:1::20");
        assert!(!sel.used_temporary);
        assert!(sel.temporary_only_ifaces.is_empty());
    }

    #[test]
    fn temporary_only_is_used_and_flagged() {
        let ifaces = [iface("wlan0", "2a00:1450:1::99", true)];
        let sel = select_public_v6(&ifaces, None);
        assert_eq!(sel.addr().unwrap().to_string(), "2a00:1450:1::99");
        assert!(sel.used_temporary);
        assert_eq!(sel.temporary_only_ifaces, vec!["wlan0"]);
    }

    #[test]
    fn selection_is_one_per_interface_and_prefers_default_route() {
        let ifaces = [
            iface("eth1", "2606:4700:2::1", false),
            iface("eth0", "2606:4700:3::1", false),
            iface("eth0", "2606:4700:3::2", false),
            iface("lo", "::1", false),
            iface("tun0", "fd00::5", false),
        ];
        let default_src = Some("2606:4700:3::2".parse().unwrap());
        let sel = select_public_v6(&ifaces, default_src);
        assert_eq!(sel.candidates.len(), 2, "one per interface");
        assert_eq!(sel.addr().unwrap().to_string(), "2606:4700:3::2");
        assert!(sel.best().unwrap().default_route);
        assert_eq!(sel.candidates[1].iface, "eth1");
    }

    #[test]
    fn no_global_v6_selects_nothing() {
        let ifaces = [
            iface("lo", "::1", false),
            iface("eth0", "fe80::1", false),
            iface("eth0", "fd00::1", false),
        ];
        assert!(select_public_v6(&ifaces, None).is_empty());
    }

    // ── IPv6: listen + publish ────────────────────────────────────────

    #[test]
    fn plan_listen_adds_wildcard_v6_and_is_idempotent() {
        let v4 = vec![ma("/ip4/0.0.0.0/tcp/30333")];
        let planned = plan_listen_addresses(&v4, true, 30333);
        assert_eq!(planned.len(), 2);
        assert!(planned.contains(&ma("/ip6/::/tcp/30333")));
        assert!(has_v6_listener(&planned));
        // Second pass adds nothing.
        assert_eq!(plan_listen_addresses(&planned, true, 30333), planned);
        // No v6 ⇒ byte-identical to today.
        assert_eq!(plan_listen_addresses(&v4, false, 30333), v4);
    }

    #[test]
    fn published_bases_include_both_stacks() {
        let v4 = ma("/ip4/203.0.113.10/tcp/30333");
        let v6: Ipv6Addr = "2606:4700:4700::1111".parse().unwrap();
        let bases = published_bases(Some(&v4), Some(&v6), 30333);
        assert_eq!(
            bases,
            vec![v4.clone(), ma("/ip6/2606:4700:4700::1111/tcp/30333")]
        );
        // No v6 ⇒ exactly the v4 entry (no behaviour change).
        assert_eq!(published_bases(Some(&v4), None, 30333), vec![v4.clone()]);
        // A non-global v6 address is never published.
        let ula: Ipv6Addr = "fd00::1".parse().unwrap();
        assert_eq!(published_bases(Some(&v4), Some(&ula), 30333), vec![v4]);
        // v6-only host publishes just the v6 entry.
        let v6only = published_bases(None, Some(&v6), 30333);
        assert_eq!(v6only, vec![ma("/ip6/2606:4700:4700::1111/tcp/30333")]);
    }

    // ── IPv6: configured endpoints and evidence ───────────────────────

    #[test]
    fn ip6_public_addr_must_match_observation_exactly() {
        let cfg = ma("/ip6/2606:4700:4700::1111/tcp/30333");
        let observed = vec![ma("/ip6/2606:4700:4700::1111/tcp/30333")];
        let pass = evaluate_with(&GateInputs {
            inbound_peers: 0,
            inbound_v6_peers: 0,
            listen: &[ma("/ip6/::/tcp/30333")],
            observed: &observed,
            configured: Some(&cfg),
            resolved: &[],
            stun_mapped: false,
            tcp_probe_ok: false,
        });
        assert!(pass.dialable, "{}", pass.detail);
        assert_eq!(pass.evidence, Some(Evidence::ConfiguredAddrObserved));

        // Same prefix, different host: not the configured endpoint.
        let wrong = vec![ma("/ip6/2606:4700:4700::2222/tcp/30333")];
        let fail = evaluate(0, &[ma("/ip6/::/tcp/30333")], &wrong, Some(&cfg));
        assert!(!fail.dialable, "{}", fail.detail);

        // Right address, wrong port: forward not open on the configured port.
        let wrong_port = vec![ma("/ip6/2606:4700:4700::1111/tcp/54321")];
        assert!(!evaluate(0, &[ma("/ip6/::/tcp/30333")], &wrong_port, Some(&cfg)).dialable);
    }

    #[test]
    fn v4_inbound_peer_does_not_prove_a_v6_endpoint() {
        let cfg = ma("/ip6/2606:4700:4700::1111/tcp/30333");
        let listen = [ma("/ip4/0.0.0.0/tcp/30333"), ma("/ip6/::/tcp/30333")];
        // Dual-stack host: peers connected, but only over IPv4. A closed v6
        // firewall must NOT be papered over.
        let v4_only_inbound = evaluate_with(&GateInputs {
            inbound_peers: 4,
            inbound_v6_peers: 0,
            listen: &listen,
            observed: &[],
            configured: Some(&cfg),
            resolved: &[],
            stun_mapped: false,
            tcp_probe_ok: false,
        });
        assert!(
            !v4_only_inbound.dialable,
            "v4 inbound peers must not prove a v6 endpoint: {}",
            v4_only_inbound.detail
        );

        // One peer that actually arrived over v6 is real evidence.
        let v6_inbound = evaluate_with(&GateInputs {
            inbound_peers: 4,
            inbound_v6_peers: 1,
            listen: &listen,
            observed: &[],
            configured: Some(&cfg),
            resolved: &[],
            stun_mapped: false,
            tcp_probe_ok: false,
        });
        assert!(v6_inbound.dialable, "{}", v6_inbound.detail);
        assert_eq!(v6_inbound.evidence, Some(Evidence::InboundPeer));

        // The v4 path is unchanged: a v4 endpoint still accepts v4 inbound.
        let v4cfg = ma("/ip4/203.0.113.9/tcp/30333");
        let v4 = evaluate_with(&GateInputs {
            inbound_peers: 1,
            inbound_v6_peers: 0,
            listen: &listen,
            observed: &[],
            configured: Some(&v4cfg),
            resolved: &[],
            stun_mapped: false,
            tcp_probe_ok: false,
        });
        assert!(v4.dialable, "{}", v4.detail);
    }

    #[test]
    fn ip6_private_or_documentation_configured_addr_fails() {
        for s in [
            "/ip6/fd00::1/tcp/30333",
            "/ip6/fe80::1/tcp/30333",
            "/ip6/2001:db8::1/tcp/30333",
            "/ip6/::ffff:8.8.8.8/tcp/30333",
        ] {
            let cfg = ma(s);
            let v = evaluate(0, &[ma("/ip6/::/tcp/30333")], &[], Some(&cfg));
            assert!(!v.dialable, "{s} must not be dialable");
            assert!(v.detail.contains("NON-public"), "{s}: {}", v.detail);
        }
    }

    #[test]
    fn dns6_is_first_class_and_matches_a_resolved_v6_observation() {
        assert_eq!(
            host_port_of(&ma("/dns6/node.example/tcp/30333")),
            Some((Host::Dns("node.example".into()), 30333))
        );
        let cfg = ma("/dns6/node.example/tcp/30333");
        let resolved: Vec<IpAddr> = vec!["2606:4700:4700::1111".parse().unwrap()];
        let observed = vec![ma("/ip6/2606:4700:4700::1111/tcp/30333")];
        let v = evaluate_with(&GateInputs {
            inbound_peers: 0,
            inbound_v6_peers: 0,
            listen: &[ma("/ip6/::/tcp/30333")],
            observed: &observed,
            configured: Some(&cfg),
            resolved: &resolved,
            stun_mapped: false,
            tcp_probe_ok: false,
        });
        assert!(v.dialable, "{}", v.detail);
        assert_eq!(v.evidence, Some(Evidence::ConfiguredAddrObserved));

        // A dns6 name resolving to a ULA is never dialable.
        let ula: Vec<IpAddr> = vec!["fd00::1".parse().unwrap()];
        let bad = evaluate_with(&GateInputs {
            inbound_peers: 0,
            inbound_v6_peers: 0,
            listen: &[ma("/ip6/::/tcp/30333")],
            observed: &[],
            configured: Some(&cfg),
            resolved: &ula,
            stun_mapped: true,
            tcp_probe_ok: true,
        });
        assert!(!bad.dialable);
    }

    #[test]
    fn observed_v6_on_the_listen_port_passes_without_config() {
        let listen = [ma("/ip6/::/tcp/30333")];
        let observed = vec![ma("/ip6/2606:4700:4700::1111/tcp/30333")];
        let v = evaluate(0, &listen, &observed, None);
        assert!(v.dialable, "{}", v.detail);
        assert_eq!(v.evidence, Some(Evidence::ObservedPublicAtListenPort));
        assert_eq!(
            observed_v6(&observed, 30333),
            Some("2606:4700:4700::1111".parse().unwrap())
        );
        // A ULA observation is not evidence.
        assert_eq!(observed_v6(&[ma("/ip6/fd00::1/tcp/30333")], 30333), None);
    }

    #[test]
    fn inbound_v6_peer_count_ignores_v4_inbound() {
        let mut connected_peers = std::collections::HashMap::new();
        connected_peers.insert(
            "v6".to_string(),
            sc_network::network_state::Peer {
                endpoint: PeerEndpoint::Listening {
                    local_addr: ma("/ip6/2606:4700:4700::1111/tcp/30333").into(),
                    send_back_addr: ma("/ip6/2606:4700:4700::2/tcp/4000").into(),
                },
                version_string: None,
                latest_ping_time: None,
                known_addresses: Default::default(),
            },
        );
        connected_peers.insert(
            "v4".to_string(),
            sc_network::network_state::Peer {
                endpoint: PeerEndpoint::Listening {
                    local_addr: ma("/ip4/203.0.113.10/tcp/30333").into(),
                    send_back_addr: ma("/ip4/198.51.100.2/tcp/4000").into(),
                },
                version_string: None,
                latest_ping_time: None,
                known_addresses: Default::default(),
            },
        );
        let state = NetworkState {
            peer_id: "local".into(),
            listened_addresses: Default::default(),
            external_addresses: Default::default(),
            connected_peers,
            not_connected_peers: Default::default(),
            peerset: serde_json::Value::Null,
        };
        assert_eq!(inbound_peer_count(&state), 2);
        assert_eq!(inbound_v6_peer_count(&state), 1);
    }

    #[test]
    fn stun_v6_xor_mapped_address_is_parsed() {
        let tid = [7u8; 12];
        // Body = 4-byte attribute header + 20-byte value (family + port + 16
        // address bytes) = 24.
        let mut msg = vec![0x01, 0x01, 0x00, 0x18];
        msg.extend_from_slice(&[0x21, 0x12, 0xA4, 0x42]);
        msg.extend_from_slice(&tid);
        msg.extend_from_slice(&[0x00, 0x20, 0x00, 0x14, 0x00, 0x02]);
        let port: u16 = 30333;
        msg.extend_from_slice(&(port ^ 0x2112).to_be_bytes());
        let mut addr = [0u8; 16];
        addr[0] = 0x26;
        addr[1] = 0x06;
        addr[15] = 0x01;
        let mut mask = [0u8; 16];
        mask[0..4].copy_from_slice(&[0x21, 0x12, 0xA4, 0x42]);
        mask[4..16].copy_from_slice(&tid);
        for i in 0..16 {
            msg.push(addr[i] ^ mask[i]);
        }
        assert_eq!(
            parse_stun_xor_mapped(&msg),
            Some((IpAddr::V6(addr.into()), 30333))
        );
    }

    // ── IPv6: prefix rotation ────────────────────────────────────────

    #[test]
    fn prefix_change_classifies_rotation_and_address_churn() {
        let a: Ipv6Addr = "2606:4700:4700::1".parse().unwrap();
        let same_prefix: Ipv6Addr = "2606:4700:4700::2".parse().unwrap();
        let rotated: Ipv6Addr = "2a00:1450:4001::1".parse().unwrap();
        assert_eq!(prefix_change(None, None), PrefixChange::None);
        assert_eq!(prefix_change(Some(a), Some(a)), PrefixChange::Same);
        assert_eq!(
            prefix_change(Some(a), Some(same_prefix)),
            PrefixChange::AddressChanged {
                old: a,
                new: same_prefix
            }
        );
        assert!(matches!(
            prefix_change(Some(a), Some(rotated)),
            PrefixChange::PrefixRotated { .. }
        ));
        assert_eq!(prefix_change(None, Some(a)), PrefixChange::Appeared(a));
        assert_eq!(prefix_change(Some(a), None), PrefixChange::Disappeared(a));
        assert!(!prefix_change(Some(a), Some(a)).needs_republish());
        assert!(prefix_change(Some(a), Some(rotated)).needs_republish());
    }

    #[test]
    fn prefix_rotation_republishes_and_drops_the_stale_address() {
        let v4 = ma("/ip4/203.0.113.10/tcp/30333");
        let old: Ipv6Addr = "2606:4700:4700::1".parse().unwrap();
        let new: Ipv6Addr = "2a00:1450:4001::9".parse().unwrap();

        let action = rotation_action(Some(&v4), Some(old), Some(new), 30333)
            .expect("a prefix rotation must republish");
        assert!(matches!(action.change, PrefixChange::PrefixRotated { .. }));
        assert_eq!(
            action.bases,
            vec![v4, ma("/ip6/2a00:1450:4001::9/tcp/30333")]
        );
        // The stale address appears nowhere in the new set.
        assert!(!action
            .bases
            .iter()
            .any(|b| b.to_string().contains("2606:4700:4700::1")));

        // No change → no republish (edge-triggered, never per tick).
        assert!(rotation_action(
            Some(&ma("/ip4/203.0.113.10/tcp/30333")),
            Some(old),
            Some(old),
            30333
        )
        .is_none());

        // Disappeared → drop v6, keep v4.
        let gone = rotation_action(
            Some(&ma("/ip4/203.0.113.10/tcp/30333")),
            Some(old),
            None,
            30333,
        )
        .expect("disappearance must republish");
        assert_eq!(gone.bases, vec![ma("/ip4/203.0.113.10/tcp/30333")]);
    }

    #[test]
    fn no_v6_keeps_todays_v4_only_behaviour() {
        // The whole v6 surface is a no-op when detection finds nothing.
        assert!(select_public_v6(&[], None).is_empty());
        let v4 = ma("/ip4/0.0.0.0/tcp/30333");
        assert_eq!(
            plan_listen_addresses(std::slice::from_ref(&v4), false, 30333),
            vec![v4.clone()]
        );
        assert_eq!(published_bases(Some(&v4), None, 30333), vec![v4]);
        assert!(!crate::dialability::is_v6_endpoint(&ma(
            "/ip4/203.0.113.10/tcp/30333"
        )));
    }

    // ── SPEC 47: reachability tiers ───────────────────────────────────────

    #[test]
    fn reachability_is_derived_from_evidence_never_declared() {
        assert_eq!(reachability_of(true), Reachability::Backbone);
        assert_eq!(reachability_of(false), Reachability::Leaf);
        assert_eq!(Reachability::Backbone.as_str(), "backbone");
        assert_eq!(Reachability::Leaf.as_str(), "leaf");
    }

    #[test]
    fn soft_gate_is_the_default_so_a_leaf_does_not_exit() {
        // Default: no --require-backbone => soft. This is the regression that
        // made stock installs exit 78 behind a home router.
        assert!(!parse_require_backbone(None));
        assert!(!parse_require_backbone(Some("0")));
        // Opt-in hard gate still parses, and the exit code is unchanged.
        for v in ["1", "true", "yes", "on"] {
            assert!(
                parse_require_backbone(Some(v)),
                "{v} must enable the hard gate"
            );
        }
        assert_eq!(EXIT_NOT_DIALABLE, 78);
    }

    #[test]
    fn only_an_explicit_hard_gate_may_exit_and_never_past_the_safety_valve() {
        // Default (soft): a leaf keeps running.
        assert!(!hard_gate_must_exit(false, false));
        // --require-backbone: still exits 78.
        assert!(hard_gate_must_exit(true, false));
        // ...unless the sole-authority valve fired, which always wins.
        assert!(!hard_gate_must_exit(true, true));
        assert!(!hard_gate_must_exit(false, true));
    }

    #[test]
    fn site_roles_are_off_unless_explicitly_enabled() {
        assert!(!parse_site_gateway(None));
        assert!(!parse_site_gateway(Some("")));
        assert!(parse_site_gateway(Some("1")));
        assert!(parse_site_gateway(Some(" TRUE ")));

        let mut cfg = GateConfig::new(true, Duration::from_secs(10), None);
        cfg.site_gateway = false;
        cfg.site_elect = false;
        assert_eq!(cfg.intended_tier(false), "leaf");
        cfg.site_gateway = true;
        assert_eq!(cfg.intended_tier(true), "gateway");
        assert_eq!(cfg.intended_tier(false), "gateway (unreachable)");
    }

    #[test]
    fn site_gateway_forces_discoverable_even_with_no_discoverable() {
        // Assemble the config the way `from_env` does, without touching the
        // process environment (tests run in parallel).
        let site_gateway = parse_site_gateway(Some("1"));
        let discoverable = site_gateway || parse_discoverable(Some("0"));
        assert!(discoverable, "a named gateway must be allowed to advertise");
    }

    #[test]
    fn portmap_hint_renders_the_four_canonical_outcomes() {
        let granted = PortMapHint::Granted {
            external: "203.0.113.9:30333".into(),
            lease_secs: 7200,
        }
        .line();
        assert!(
            granted.contains("granted -> 203.0.113.9:30333"),
            "{granted}"
        );
        assert!(granted.contains("lease 120 min"), "{granted}");

        let refused = PortMapHint::Refused("no IGD found".into()).line();
        assert!(
            refused.contains("refused by router (no IGD found)"),
            "{refused}"
        );

        let disabled = PortMapHint::Disabled.line();
        assert_eq!(disabled, "portmap: disabled (GPN_UPNP=off)");

        let skipped = PortMapHint::Skipped.line();
        assert_eq!(skipped, "portmap: skipped (public address configured)");

        // CGNAT is a refusal with the reason spelled out.
        let cgnat = PortMapHint::Cgnat("100.64.1.2:30333".into()).line();
        assert!(cgnat.contains("CGNAT"), "{cgnat}");
        for line in [&granted, &refused, &cgnat] {
            assert!(line.is_ascii(), "{line}");
        }
    }

    #[test]
    fn degradation_notice_carries_the_portmap_outcome() {
        let lan: IpAddr = "192.168.1.42".parse().unwrap();
        let note = degradation_notice(
            Some(lan),
            30333,
            true,
            false,
            &PortMapHint::Refused("UPnP disabled on router".into()),
        )
        .expect("soft notice");
        assert!(
            note.contains("portmap: refused by router (UPnP disabled on router)"),
            "the notice must say WHY the port is closed: {note}"
        );
        // Exactly one degradation block, not one per attempted rung.
        assert_eq!(note.matches("YOUR NODE IS NOT DIALABLE").count(), 1);
        assert!(note.is_ascii(), "{note}");
    }

    fn sample(peer: &str, first: u64, last: u64, witnesses: u32) -> UptimeSample {
        UptimeSample {
            peer_id: peer.into(),
            first_witnessed_secs: first,
            last_witnessed_secs: last,
            witnesses,
        }
    }

    #[test]
    fn election_uses_only_witnessed_uptime_and_is_deterministic() {
        let a = sample("peer-a", 0, 1000, 2);
        let b = sample("peer-b", 0, 1000, 2);
        let c = sample("peer-c", 0, 10, 4);
        // Equal score breaks on the smallest peer id, independently on every node.
        assert_eq!(
            elect_gateway(&[b.clone(), a.clone()]).unwrap().peer_id,
            "peer-a"
        );
        assert_eq!(
            elect_gateway(&[a.clone(), b.clone()]).unwrap().peer_id,
            "peer-a"
        );
        // Witness duration beats a short window, regardless of witness count.
        assert_eq!(elect_gateway(&[c, a.clone()]).unwrap().peer_id, "peer-a");
        assert!(elect_gateway(&[]).is_none());

        // Witness count is capped so many sessions from one peer cannot dominate.
        let one = sample("x", 0, 100, 1);
        let spam = sample("y", 0, 100, 1000);
        assert_eq!(one.score(), 100);
        assert_eq!(spam.score(), 400);
    }

    // ── Socket ownership: one verified gateway per public IP ────────

    fn claim(socket: &str, peer: &str, verifier: &str, egress: &str, at: u64) -> PublicClaim {
        PublicClaim::new(socket, peer, verifier, egress, at, 600)
    }

    #[test]
    fn same_egress_verifier_never_passes() {
        // The AP runs on the seed box, so it shares the claimant's egress. It
        // must report `skipped`, never `verified` — that is the one check that
        // would have prevented the 2026-11-12 double-claim on 142.129.54.88.
        let c = claim(
            "142.129.54.88:30333",
            "12D3KooWA",
            "12D3KooWAP",
            "142.129.54.88",
            100,
        );
        let view = VerifierView {
            verifier_id: "12D3KooWAP",
            verifier_egress: "142.129.54.88",
            observed_peer_id: Some("12D3KooWA"),
        };
        let out = judge_claim(&c, &view);
        assert_eq!(out.code(), "skipped");
        assert!(!out.is_verified(), "same-egress is not proof: {out:?}");
        assert!(out.summary().contains("same-egress"));
    }

    #[test]
    fn wrong_peer_is_never_a_pass_and_reports_the_id() {
        // Box B dials wan:30333 and reaches box A's libp2p listener: the
        // handshake succeeds, so a bare TCP check would "prove" dialability.
        // The peer id is what falsifies the claim.
        let c = claim(
            "142.129.54.88:30333",
            "12D3KooWB",
            "12D3KooWAP",
            "23.241.109.255",
            100,
        );
        let view = VerifierView {
            verifier_id: "12D3KooWAP",
            verifier_egress: "23.241.109.255",
            observed_peer_id: Some("12D3KooWA"),
        };
        let out = judge_claim(&c, &view);
        assert_eq!(out.code(), "wrong-peer");
        assert!(!out.is_verified());
        // T14: the reason names the id actually reached.
        assert!(out.summary().contains("12D3KooWA"), "{out:?}");
        assert!(out.summary().contains("12D3KooWB"), "{out:?}");

        // No handshake at all is `unreachable`, not a pass either.
        let out = judge_claim(
            &c,
            &VerifierView {
                observed_peer_id: None,
                ..view
            },
        );
        assert_eq!(out.code(), "unreachable");
    }

    #[test]
    fn newest_verified_claim_wins_and_the_loser_demotes_without_a_restart() {
        // The forward moved A → B. Both claims are verification outcomes; only
        // one can hold the socket at a time.
        let mut reg = SocketRegistry::new();
        let a = claim(
            "142.129.54.88:30333",
            "12D3KooWA",
            "v",
            "23.241.109.255",
            100,
        );
        let b = claim(
            "142.129.54.88:30333",
            "12D3KooWB",
            "v",
            "23.241.109.255",
            110,
        );
        let ok = DialbackOutcome::Verified {
            peer_id: "x".into(),
        };
        assert_eq!(reg.offer(a.clone(), &ok, 100), ClaimDecision::Accepted);
        assert!(reg.holds("142.129.54.88:30333", "12D3KooWA", 100));
        assert_eq!(
            reg.offer(b.clone(), &ok, 110),
            ClaimDecision::Superseded {
                previous: "12D3KooWA".into()
            }
        );
        assert!(reg.holds("142.129.54.88:30333", "12D3KooWB", 110));
        assert!(!reg.holds("142.129.54.88:30333", "12D3KooWA", 110));

        // Expiry: no heartbeat ⇒ no owner ⇒ nothing publishable (a lease, not
        // a fact written once).
        assert!(reg.owner("142.129.54.88:30333", 710).is_none());

        // An unverified claim is refused outright.
        let mut reg = SocketRegistry::new();
        assert_eq!(
            reg.offer(
                claim("142.129.54.88:30333", "12D3KooWA", "v", "e", 100),
                &DialbackOutcome::Skipped {
                    reason: "same-egress".into()
                },
                100
            ),
            ClaimDecision::Rejected {
                reason: "skipped".into()
            }
        );
        assert!(reg.live_claims(100).is_empty());
    }

    #[test]
    fn two_boxes_behind_one_nat_cannot_both_be_gateway() {
        // The live /providers rows: two peer ids, one socket, both backbone.
        let mut cfg = GateConfig::new(true, Duration::from_secs(10), None);
        cfg.site_elect = true;
        let a = claim(
            "142.129.54.88:30333",
            "12D3KooWA",
            "v",
            "23.241.109.255",
            100,
        );
        let b = claim(
            "142.129.54.88:30333",
            "12D3KooWB",
            "v",
            "23.241.109.255",
            110,
        );

        // Only A's claim is visible: A is the gateway.
        assert_eq!(
            gateway_role(
                &cfg,
                "12D3KooWA",
                Some("142.129.54.88:30333"),
                std::slice::from_ref(&a),
                &[],
                100
            ),
            GatewayRole::Gateway
        );
        // The forward moved to B: the newest verified claim wins, and B is the
        // gateway while A has no live claim → Leaf. Neither node restarts.
        assert_eq!(
            gateway_role(
                &cfg,
                "12D3KooWB",
                Some("142.129.54.88:30333"),
                &[a.clone(), b.clone()],
                &[],
                110
            ),
            GatewayRole::Gateway
        );
        assert_eq!(
            gateway_role(
                &cfg,
                "12D3KooWA",
                Some("142.129.54.88:30333"),
                &[a, b],
                &[],
                110
            ),
            GatewayRole::Leaf,
            "the loser must demote on its own next cycle"
        );
    }

    #[test]
    fn election_requires_a_verified_claim_and_uptime_only_ranks() {
        let mut cfg = GateConfig::new(true, Duration::from_secs(10), None);
        cfg.discoverable = true;
        // Election off: exactly today's rule (explicit naming).
        assert_eq!(
            gateway_role(&cfg, "me", Some("1.2.3.4:30333"), &[], &[], 100),
            GatewayRole::Backbone
        );
        cfg.site_gateway = true;
        assert_eq!(
            gateway_role(&cfg, "me", Some("1.2.3.4:30333"), &[], &[], 100),
            GatewayRole::Gateway
        );

        // Election on: no verified claim ⇒ nothing publishable, even for an
        // operator who "knows" their address (design §4.1.9).
        let cfg = GateConfig {
            site_elect: true,
            site_gateway: false,
            ..cfg
        };
        assert_eq!(
            gateway_role(&cfg, "me", Some("1.2.3.4:30333"), &[], &[], 100),
            GatewayRole::Leaf
        );
        let mine = claim("1.2.3.4:30333", "me", "v", "9.9.9.9", 100);
        assert_eq!(
            gateway_role(
                &cfg,
                "me",
                Some("1.2.3.4:30333"),
                std::slice::from_ref(&mine),
                &[],
                100
            ),
            GatewayRole::Gateway
        );

        // Two verified sockets behind one public IP: the witnessed-uptime
        // winner is the site's dial target; the other stays a verified backbone
        // (it is dialable, just not the LAN's one target).
        let other = claim("1.2.3.4:30334", "other", "v", "9.9.9.9", 100);
        let samples = vec![sample("me", 0, 1000, 2), sample("other", 0, 10, 1)];
        assert_eq!(
            gateway_role(
                &cfg,
                "me",
                Some("1.2.3.4:30333"),
                &[mine.clone(), other.clone()],
                &samples,
                100
            ),
            GatewayRole::Gateway
        );
        assert_eq!(
            gateway_role(
                &cfg,
                "other",
                Some("1.2.3.4:30334"),
                &[mine, other],
                &samples,
                100
            ),
            GatewayRole::Backbone
        );
    }

    #[test]
    fn claims_file_is_parsed_strictly_and_a_missing_file_is_no_claims() {
        let dir = std::env::temp_dir().join(format!(
            "gpn-claims-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("claims.json");
        // Missing file ⇒ no claims (Leaf under election), never a guess.
        assert!(read_claims_file(&path).is_empty());
        std::fs::write(
            &path,
            serde_json::json!([
                {"socket": "1.2.3.4:30333", "peer_id": "12D3KooWA",
                 "verified_at": 100, "verifier_id": "v",
                 "verifier_egress": "9.9.9.9", "expires_at": 700},
                {"socket": "", "peer_id": "12D3KooWB",
                 "verified_at": 100, "expires_at": 700},
                {"garbage": true}
            ])
            .to_string(),
        )
        .unwrap();
        let claims = read_claims_file(&path);
        assert_eq!(claims.len(), 1, "incomplete entries are dropped");
        assert_eq!(claims[0].socket, "1.2.3.4:30333");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn election_requires_opt_in_and_a_won_comparison() {
        let candidates = vec![sample("me", 0, 500, 2), sample("other", 0, 100, 2)];
        let mut cfg = GateConfig::new(true, Duration::from_secs(10), None);
        cfg.site_elect = false;
        assert!(!elected_as_gateway(&cfg, "me", &candidates));
        cfg.site_elect = true;
        assert!(elected_as_gateway(&cfg, "me", &candidates));
        assert!(!elected_as_gateway(&cfg, "other", &candidates));
    }

    #[test]
    fn a_leaf_state_file_never_carries_a_dialable_multiaddr() {
        // `write_state_file` is the last line of defence: even if a caller
        // passes endpoints, a leaf writes an empty list.
        let dir = std::env::temp_dir().join(format!(
            "gpn-spec47-leaf-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _guard = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("GPN_DIALABILITY_STATE", dir.join("dialability.json"));
        let pubn = StatePublication {
            public: Some(ma("/ip4/203.0.113.9/tcp/30333")),
            public6: None,
            source: "test".into(),
            peer_id: Some("12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo".into()),
            probe_peer_id: None,
        };
        write_state_file(&pubn, false, true, "not reachable");
        let text = std::fs::read_to_string(dir.join("dialability.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["reachability"], "leaf");
        assert_eq!(v["dialable"], false);
        assert_eq!(v["multiaddrs"], serde_json::json!([]));
        assert!(v["public_endpoint"].is_null());
        // The gateway flag is role metadata, not a dialability claim.
        assert_eq!(v["gateway"], true);
        // The verdict names its owner, and the probe identity defaults to it.
        assert_eq!(
            v["peer_id"],
            "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo"
        );
        assert_eq!(v["probe_peer_id"], v["peer_id"]);

        // A backbone publishes the endpoint it proved.
        write_state_file(&pubn, true, false, "inbound peer");
        let text = std::fs::read_to_string(dir.join("dialability.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["reachability"], "backbone");
        assert_eq!(
            v["multiaddrs"],
            serde_json::json!(["/ip4/203.0.113.9/tcp/30333"])
        );
        std::env::remove_var("GPN_DIALABILITY_STATE");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    // The lock is held across the await on purpose: it serialises a
    // process-global env var against another test, and the guarded region is a
    // few milliseconds in a test only.
    #[allow(clippy::await_holding_lock)]
    async fn the_recheck_promotes_a_leaf_that_becomes_reachable() {
        // A node that started as a leaf, then gained an inbound peer. The
        // re-check must republish it as backbone - this is the whole point of
        // not exiting at startup: NAT can open later.
        let dir = std::env::temp_dir().join(format!(
            "gpn-spec47-promote-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let state = dir.join("dialability.json");
        let _guard = STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("GPN_DIALABILITY_STATE", &state);

        let net = std::sync::Arc::new(StubNet::new(1, vec![ma("/ip4/0.0.0.0/tcp/30333")], vec![]));
        let cfg = GateConfig::new(true, Duration::from_secs(1), None);
        let publication = StatePublication {
            public: Some(ma("/ip4/203.0.113.9/tcp/30333")),
            public6: None,
            source: "recheck-test".into(),
            peer_id: Some("12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo".into()),
            probe_peer_id: None,
        };
        let task = tokio::spawn(watch_dialability_with_interval(
            net,
            cfg,
            false,
            publication,
            Duration::from_millis(20),
        ));
        // Give the loop at least two ticks.
        tokio::time::sleep(Duration::from_millis(120)).await;
        task.abort();

        let text = std::fs::read_to_string(&state).expect("re-check must write state");
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            v["reachability"], "backbone",
            "leaf must be promoted: {text}"
        );
        assert_eq!(v["dialable"], true);
        assert_eq!(
            v["multiaddrs"],
            serde_json::json!(["/ip4/203.0.113.9/tcp/30333"]),
            "promotion must publish the proved endpoint"
        );

        std::env::remove_var("GPN_DIALABILITY_STATE");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
