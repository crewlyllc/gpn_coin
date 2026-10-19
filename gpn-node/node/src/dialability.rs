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
//! # Behaviour (v2 — seed-by-default, *non-fatal*)
//!
//! A node is a **backbone node by default**: it announces its endpoint and
//! accepts inbound peers. But dialability is a **capability probe, not a
//! membership gate** — a node that cannot be dialled (CGNAT, no forward) still
//! syncs, mines and serves AI jobs. It only *degrades* to non-discoverable.
//!
//! * Probe is ON by default. `GPN_REQUIRE_DIALABLE=0` (also `false`/`no`/`off`)
//!   skips the probe entirely (local/single-node testing).
//! * On failure the node logs the actionable [`degradation_notice`] block once
//!   and **continues** as a non-discoverable participant.
//! * `GPN_REQUIRE_BACKBONE=1` (or `--require-backbone`) restores the old hard
//!   gate: log the error and **exit with [`EXIT_NOT_DIALABLE`] (78)**.
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

/// Extract `(ip, port)` from a multiaddress, ignoring `/p2p/…` and any other
/// trailing components. Returns `None` unless both an IP and a non-zero
/// TCP/UDP port are present.
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
}

impl std::fmt::Display for Evidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Evidence::InboundPeer => "inbound peer connection",
            Evidence::ConfiguredAddrObserved => "configured public address observed",
            Evidence::PublicListenAddr => "public listen address",
            Evidence::ObservedPublicAtListenPort => "observed public address on listen port",
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
        Verdict { dialable: true, evidence: Some(evidence), detail: detail.into() }
    }

    fn fail(detail: impl Into<String>) -> Self {
        Verdict { dialable: false, evidence: None, detail: detail.into() }
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

/// The pure gate decision.
///
/// * `inbound_peers` — peers that dialed us (see [`inbound_peer_count`]).
/// * `listen` — the node's listen addresses (`NetworkStateInfo::listen_addresses`).
/// * `observed` — addresses peers reported for us (`external_addresses`).
/// * `configured` — `GPN_PUBLIC_ADDR` / `--public-addr`, if any.
pub fn evaluate(
    inbound_peers: usize,
    listen: &[Multiaddr],
    observed: &[Multiaddr],
    configured: Option<&Multiaddr>,
) -> Verdict {
    if inbound_peers > 0 {
        return Verdict::pass(
            Evidence::InboundPeer,
            format!("{inbound_peers} peer(s) dialed this node (inbound connection)"),
        );
    }

    // A configured address is authoritative: require an exact ip:port match.
    if let Some(cfg_ma) = configured {
        match endpoint_of(cfg_ma) {
            Some((ip, port)) if classify_ip(&ip).is_public() => {
                for o in observed {
                    if endpoint_of(o) == Some((ip, port)) {
                        return Verdict::pass(
                            Evidence::ConfiguredAddrObserved,
                            format!("observed external address matches GPN_PUBLIC_ADDR ({cfg_ma})"),
                        );
                    }
                }
                for l in listen {
                    if endpoint_of(l) == Some((ip, port)) {
                        return Verdict::pass(
                            Evidence::PublicListenAddr,
                            format!("listening directly on the public address {cfg_ma}"),
                        );
                    }
                }
                return Verdict::fail(format!(
                    "configured public address {cfg_ma} was neither observed by peers nor bound \
                     locally — the port is probably not forwarded (observed: [{}], listening: [{}])",
                    join(observed),
                    join(listen)
                ));
            }
            Some((ip, _)) => {
                return Verdict::fail(format!(
                    "GPN_PUBLIC_ADDR {cfg_ma} uses a NON-public IP {ip} ({}); a public \
                     (globally routable) address is required",
                    class_desc(classify_ip(&ip))
                ));
            }
            None => {
                return Verdict::fail(format!(
                    "GPN_PUBLIC_ADDR {cfg_ma} is not a usable /ip4|ip6/<addr>/tcp/<port> \
                     multiaddress"
                ));
            }
        }
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

    let listen_ports: Vec<u16> = listen.iter().filter_map(|l| endpoint_of(l).map(|(_, p)| p)).collect();
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
    addrs.iter().map(|a| a.to_string()).collect::<Vec<_>>().join(", ")
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

/// Gate configuration, resolved from the environment (+ `--public-addr`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateConfig {
    /// Whether the capability probe runs at all. Default: `true`.
    pub require: bool,
    /// Hard gate (`--require-backbone`): exit [`EXIT_NOT_DIALABLE`] when the
    /// probe fails. Default: `false` (degrade gracefully).
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

/// Parse `GPN_REQUIRE_BACKBONE`. Default **OFF** — failing the probe degrades,
/// it does not stop the node.
pub fn parse_require_backbone(raw: Option<&str>) -> bool {
    match raw.map(|s| s.trim().to_ascii_lowercase()) {
        Some(v) if matches!(v.as_str(), "1" | "true" | "yes" | "on") => true,
        _ => false,
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

/// The operator-facing degradation notice (design §5.2), or `None` when the
/// operator opted out of seed publication.
///
/// Emitted **once per start**, after any NAT-mapping attempt (none in this
/// build — see queue #46 Phase 1), and never in the log loop.
pub fn degradation_notice(lan_ip: Option<IpAddr>, port: u16, seed: bool) -> Option<String> {
    if !seed {
        return None;
    }
    let lan = lan_ip
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "<this host's LAN IP>".to_string());
    Some(format!(
        "\
================================================================================
  YOUR NODE IS NOT DIALABLE
================================================================================
Other nodes cannot open a connection to this machine on TCP {port}.

Your node is running and fully functional: it syncs, mines and serves AI jobs.
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

  3. Allow the port in this machine's firewall.
     Windows (Administrator PowerShell):
       New-NetFirewallRule -DisplayName \"GPN node\" -Direction Inbound `
         -Protocol TCP -LocalPort {port} -Action Allow
     Linux (ufw):
       sudo ufw allow {port}/tcp
     Linux (firewalld):
       sudo firewall-cmd --add-port={port}/tcp --permanent && sudo firewall-cmd --reload

  4. UPnP/NAT-PMP mapping is not enabled in this build; use step 2 (or set
     GPN_PUBLIC_ADDR if a forward maps a different external port).

  5. CGNAT: if your router's WAN address is in 100.64.0.0/10, no port forward
     can work. Run the node on a cheap VPS, or keep participating as-is.
     This is a supported outcome, not an error.

Full guide with per-OS detail:
  https://gpn.network/docs/run-a-node#make-your-node-dialable

Restart the node after fixing the forward; detection runs at startup.

Make this fatal instead:   --require-backbone
Run private, silence:      --no-seed   (or GPN_SEED=0)
================================================================================"
    ))
}

/// Wait until the node is demonstrably dialable, or the grace window expires.
///
/// Polls the live network every [`POLL_SECS`] seconds and evaluates
/// [`evaluate`]. Returns the final verdict (never blocks longer than
/// `cfg.timeout`).
pub async fn wait_for_dialability<N>(network: &N, cfg: &GateConfig) -> Verdict
where
    N: NetworkStatusProvider + NetworkStateInfo,
{
    let started = Instant::now();
    let deadline = started + cfg.timeout;
    let mut last_progress = started;
    let mut verdict;

    loop {
        let inbound = match network.network_state().await {
            Ok(state) => inbound_peer_count(&state),
            Err(_) => 0,
        };
        let listen = network.listen_addresses();
        let observed = network.external_addresses();
        verdict = evaluate(inbound, &listen, &observed, cfg.public_addr.as_ref());

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

        let sleep = POLL_SECS.min(deadline.saturating_duration_since(Instant::now()).as_secs().max(1));
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
        assert_eq!(classify_ip(&IpAddr::V4(Ipv4Addr::LOCALHOST)), IpClass::Loopback);
        assert_eq!(classify_ip(&IpAddr::V4(Ipv4Addr::UNSPECIFIED)), IpClass::Unspecified);
        assert_eq!(
            classify_ip(&"169.254.10.20".parse().unwrap()),
            IpClass::LinkLocal
        );
        assert_eq!(classify_ip(&IpAddr::V6(Ipv6Addr::LOCALHOST)), IpClass::Loopback);
        assert_eq!(classify_ip(&IpAddr::V6(Ipv6Addr::UNSPECIFIED)), IpClass::Unspecified);
        assert_eq!(
            classify_ip(&"fe80::1".parse().unwrap()),
            IpClass::LinkLocal
        );
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
        let ma: Multiaddr = "/ip4/1.2.3.4/tcp/30333/p2p/12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN"
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
        let pass = evaluate(0, &[ma("/ip4/0.0.0.0/tcp/30333")], &[ma("/ip4/1.2.3.4/tcp/30333")], Some(&cfg));
        assert!(pass.dialable);
        assert_eq!(pass.evidence, Some(Evidence::ConfiguredAddrObserved));

        // Right IP, wrong port → the forward is not open on the configured port.
        let fail = evaluate(0, &[ma("/ip4/0.0.0.0/tcp/30333")], &[ma("/ip4/1.2.3.4/tcp/54321")], Some(&cfg));
        assert!(!fail.dialable);
    }

    #[test]
    fn configured_private_addr_fails() {
        let cfg = ma("/ip4/192.168.1.5/tcp/30333");
        let v = evaluate(0, &[ma("/ip4/192.168.1.5/tcp/30333")], &[], Some(&cfg));
        assert!(!v.dialable);
        assert!(v.detail.contains("NON-public"), "detail: {}", v.detail);
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
        assert!(!parse_require_backbone(None));
        assert!(!parse_require_backbone(Some("0")));
        assert!(!parse_require_backbone(Some("false")));
        assert!(parse_require_backbone(Some("1")));
        assert!(parse_require_backbone(Some("on")));
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
        let note = degradation_notice(Some(lan), 30333, true).expect("notice when seeding");
        assert!(note.contains("YOUR NODE IS NOT DIALABLE"), "{note}");
        assert!(note.contains("192.168.1.42"), "LAN IP must be filled in: {note}");
        assert!(note.contains("External port ....... 30333"), "port: {note}");
        assert!(note.contains("--no-seed"), "must document the opt-out: {note}");
        assert!(note.contains("--require-backbone"), "must document the hard gate: {note}");
        assert!(note.contains("100.64.0.0/10"), "must explain CGNAT: {note}");
        assert!(note.contains("ufw allow 30333/tcp"), "must show the firewall step: {note}");

        // Unknown LAN IP still yields a usable block (placeholder, not a panic).
        let generic = degradation_notice(None, 30333, true).unwrap();
        assert!(generic.contains("LAN IP"), "{generic}");

        // The operator asked to be private: never nag.
        assert!(degradation_notice(Some(lan), 30333, false).is_none());
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
            StubNet { inbound, listen, observed, polls: AtomicUsize::new(0) }
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
        assert_eq!(net.polls.load(Ordering::SeqCst), 1, "should return on first poll");
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
