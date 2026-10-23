//! Automatic NAT port mapping — **UPnP-IGD first, NAT-PMP second**.
//!
//! The dialability gate ([`crate::dialability`]) requires a public, forwarded
//! port. Almost every home node sits behind a NAT router, and asking a
//! non-expert operator to open a port by hand is where seed-by-default dies
//! (design §5.5). So before the gate decides "not dialable", the node asks the
//! router to open the port for it:
//!
//! 1. **UPnP-IGD** (`igd-next`): SSDP discover the gateway, `GetExternalIPAddress`
//!    to learn the WAN address, then `AddPortMapping` TCP `port → this host`.
//! 2. **NAT-PMP** (hand-rolled, RFC 6886): same idea over UDP/5351, for Apple
//!    AirPort and routers that implement NAT-PMP but not UPnP.
//!
//! # Consent
//!
//! Mapping is on by default because the product default is "be a backbone
//! node", but it is **never silent**: the attempt and its result are logged at
//! `info`/`warn`, including the external `ip:port` the router reported. The log
//! line is the consent record. `GPN_UPNP=off` (or `--no-upnp`) disables every
//! router interaction — the node then behaves exactly as before.
//!
//! # CGNAT
//!
//! If the router's WAN address is itself private (RFC1918) or in
//! 100.64.0.0/10 (carrier-grade NAT), no port forward can help. We detect that
//! **before** asking for a mapping and report [`MapOutcome::Cgnat`] so the
//! notice can say so instead of sending the operator on a wild goose chase.
//!
//! # Truth model
//!
//! A successful mapping is **not** proof of dialability. It only means the
//! router accepted the rule. The gate still requires an inbound peer (or an
//! equivalent observation) — see [`crate::dialability::evaluate`]. The mapping
//! contributes an *advertised* address, which the gate can then confirm.
//!
//! # Address advertisement
//!
//! The mapping is attempted **before** `build_network`, so the learnt external
//! address (router WAN IP + the port the router actually assigned, which may
//! differ from the internal port) can be injected into
//! `NetworkConfiguration::public_addresses`. `GPN_PUBLIC_ADDR` /
//! `--public-addr` remains the top override and is never replaced.
//! libp2p's public-address set is fixed at construction, so an address change
//! after startup is logged loudly but only re-advertised on the next restart.

use crate::dialability::{classify_ip, IpClass};
use sc_network::multiaddr::{Multiaddr, Protocol};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::Duration;

/// Default mapping lease: one hour, renewed at half-life.
pub const DEFAULT_LEASE_SECS: u32 = 3600;

/// How often a node with **no usable mapping** re-detects a router (design
/// §5.3/§5.5: "re-check every 10 minutes").
///
/// This is the answer to the two cases a startup-only attempt cannot handle:
/// the operator enables UPnP *after* starting the node, and the router reboots
/// (or changes its WAN address) and drops the rule. Neither needs a node
/// restart any more.
pub const RECOVERY_SECS: u64 = 600;

/// UPnP SSDP discovery budget (design §5.1: ~10 s total).
pub const UPNP_SEARCH_TIMEOUT: Duration = Duration::from_secs(5);

/// NAT-PMP is UDP and quick: two attempts, 400 ms each.
const NATPMP_ATTEMPTS: u32 = 3;
const NATPMP_TIMEOUT: Duration = Duration::from_millis(400);
/// IANA-assigned NAT-PMP port.
const NATPMP_PORT: u16 = 5351;

/// Which protocol produced a mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapMethod {
    /// UPnP-IGD (SOAP `AddPortMapping`).
    UpnpIgd,
    /// NAT-PMP (RFC 6886) / PCP-class router.
    NatPmp,
}

impl std::fmt::Display for MapMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            MapMethod::UpnpIgd => "UPnP-IGD",
            MapMethod::NatPmp => "NAT-PMP",
        })
    }
}

/// A live port mapping: what the router reports, not what we wished for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapping {
    /// The `host:port` the router forwards *to* (our LAN address + listen port).
    pub internal: SocketAddr,
    /// The `ip:port` the internet sees. `external.port()` may differ from
    /// `internal.port()` — the advertised address must use the external one.
    pub external: SocketAddr,
    /// The protocol that created the rule.
    pub method: MapMethod,
    /// How long the router will keep the rule without a renewal.
    pub lease: Duration,
    /// The router's LAN address (the mapping target's gateway).
    pub gateway: IpAddr,
}

/// WAN address classification (the router's view of the internet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WanClass {
    /// Globally routable: a forward can work.
    Public,
    /// 100.64.0.0/10: carrier-grade NAT. Terminal, not an error.
    Cgnat,
    /// RFC1918/ULA: double NAT or a router that reports junk. No forward works.
    Private,
    /// Loopback/link-local/unspecified — never a WAN address.
    Unusable,
}

/// Classify the WAN address a router reports.
pub fn classify_wan(ip: &IpAddr) -> WanClass {
    if is_cgnat(ip) {
        return WanClass::Cgnat;
    }
    match classify_ip(ip) {
        IpClass::Public => WanClass::Public,
        IpClass::Private => WanClass::Private,
        _ => WanClass::Unusable,
    }
}

/// `true` iff `ip` is in 100.64.0.0/10 (RFC 6598, carrier-grade NAT).
pub fn is_cgnat(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 100 && (64..=127).contains(&o[1])
        }
        IpAddr::V6(_) => false,
    }
}

/// Why a mapping attempt failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapError {
    /// No IGD/NAT-PMP gateway answered.
    NoGateway(String),
    /// The router answered but refused / does not implement the action.
    Refused(String),
    /// Transport error (socket, HTTP, malformed response).
    Io(String),
}

impl std::fmt::Display for MapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MapError::NoGateway(s) => write!(f, "no gateway: {s}"),
            MapError::Refused(s) => write!(f, "router refused: {s}"),
            MapError::Io(s) => write!(f, "transport error: {s}"),
        }
    }
}

/// Result of the mapping ladder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapOutcome {
    /// A rule exists; `Mapping` is what the router reported.
    Mapped(Mapping),
    /// The WAN address is CGNAT/private: no forward can ever work.
    Cgnat {
        /// The WAN address the router reported.
        wan: IpAddr,
        /// How we learnt it.
        method: MapMethod,
    },
    /// Mapping is disabled by configuration. Nothing was sent to the router.
    Disabled,
    /// No router answered, or it refused. The dialability notice applies.
    Failed {
        /// Human-readable reason, already log-safe.
        reason: String,
    },
}

/// A blocking port-mapping backend. Implemented for UPnP-IGD and NAT-PMP.
///
/// The trait is blocking on purpose: both protocols are request/response over
/// UDP/HTTP with short timeouts. The async caller wraps it in
/// [`tokio::task::spawn_blocking`].
pub trait PortMapper: Send + Sync {
    /// Which protocol this backend speaks.
    fn method(&self) -> MapMethod;
    /// The gateway this backend talks to.
    fn gateway(&self) -> IpAddr;
    /// Ask the router for its WAN IPv4 address.
    fn wan_ip(&self) -> Result<IpAddr, MapError>;
    /// Create (or renew) a TCP mapping `external_port → internal`.
    /// Returns the external port the router actually assigned.
    fn add_tcp(
        &self,
        internal: SocketAddr,
        external_port: u16,
        lease: Duration,
    ) -> Result<u16, MapError>;
    /// Remove the rule (best-effort; leases expire anyway). Reserved for the
    /// clean-shutdown path and manual diagnostics.
    #[allow(dead_code)]
    fn delete_tcp(&self, external_port: u16) -> Result<(), MapError>;
}

// ── UPnP-IGD ─────────────────────────────────────────────────────────────

/// UPnP-IGD backend (owned `igd_next::Gateway`).
pub struct IgdMapper {
    gateway: igd_next::Gateway,
}

impl IgdMapper {
    /// Discover an IGD via SSDP within [`UPNP_SEARCH_TIMEOUT`].
    pub fn discover() -> Result<Self, MapError> {
        let opts = igd_next::SearchOptions {
            timeout: Some(UPNP_SEARCH_TIMEOUT),
            ..Default::default()
        };
        igd_next::search_gateway(opts)
            .map(|gateway| IgdMapper { gateway })
            .map_err(|e| MapError::NoGateway(e.to_string()))
    }
}

impl PortMapper for IgdMapper {
    fn method(&self) -> MapMethod {
        MapMethod::UpnpIgd
    }

    fn gateway(&self) -> IpAddr {
        self.gateway.addr.ip()
    }

    fn wan_ip(&self) -> Result<IpAddr, MapError> {
        self.gateway
            .get_external_ip()
            .map_err(|e| MapError::Io(e.to_string()))
    }

    fn add_tcp(
        &self,
        internal: SocketAddr,
        external_port: u16,
        lease: Duration,
    ) -> Result<u16, MapError> {
        // NOTE: try the *requested* external port first (port-preserving is
        // what the notice recommends), and fall back to any port only if the
        // router says that specific port is taken. A non-preserving mapping is
        // still useful: we advertise the external port we were given.
        match self.gateway.add_port(
            igd_next::PortMappingProtocol::TCP,
            external_port,
            internal,
            lease.as_secs() as u32,
            "GPN node (p2p)",
        ) {
            Ok(()) => Ok(external_port),
            Err(igd_next::AddPortError::PortInUse) => self
                .gateway
                .add_any_port(
                    igd_next::PortMappingProtocol::TCP,
                    internal,
                    lease.as_secs() as u32,
                    "GPN node (p2p)",
                )
                .map_err(|e| MapError::Refused(e.to_string())),
            Err(e) => Err(MapError::Refused(e.to_string())),
        }
    }

    fn delete_tcp(&self, external_port: u16) -> Result<(), MapError> {
        self.gateway
            .remove_port(igd_next::PortMappingProtocol::TCP, external_port)
            .map_err(|e| MapError::Io(e.to_string()))
    }
}

// ── NAT-PMP (RFC 6886) ───────────────────────────────────────────────────

/// NAT-PMP backend. PCP-only routers that also speak NAT-PMP work too; pure
/// PCP (RFC 6887) is not implemented.
pub struct NatPmpMapper {
    gateway: IpAddr,
}

impl NatPmpMapper {
    /// Build a mapper for a known gateway address.
    pub fn new(gateway: IpAddr) -> Self {
        NatPmpMapper { gateway }
    }

    /// Discover the default IPv4 gateway from the OS routing table.
    ///
    /// Linux reads `/proc/net/route`; macOS/BSD shells out to `route -n get
    /// default`; Windows returns `None` (use `GPN_NATPMP_GATEWAY` there).
    pub fn discover() -> Option<Self> {
        default_gateway_v4().map(NatPmpMapper::new)
    }

    fn request(&self, payload: &[u8], expect_opcode: u8) -> Result<Vec<u8>, MapError> {
        let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
        let target = SocketAddr::new(self.gateway, NATPMP_PORT);
        let mut last = MapError::NoGateway(format!(
            "no NAT-PMP response from {}:{}",
            self.gateway, NATPMP_PORT
        ));
        for _ in 0..NATPMP_ATTEMPTS {
            let sock = match UdpSocket::bind(bind) {
                Ok(s) => s,
                Err(e) => return Err(MapError::Io(e.to_string())),
            };
            if let Err(e) = sock.set_read_timeout(Some(NATPMP_TIMEOUT)) {
                return Err(MapError::Io(e.to_string()));
            }
            if let Err(e) = sock.connect(target) {
                return Err(MapError::Io(e.to_string()));
            }
            if let Err(e) = sock.send(payload) {
                last = MapError::Io(e.to_string());
                continue;
            }
            let mut buf = [0u8; 32];
            match sock.recv(&mut buf) {
                Ok(n) if n >= 8 => {
                    if buf[0] != 0 {
                        last = MapError::Refused(format!("unsupported NAT-PMP version {}", buf[0]));
                        continue;
                    }
                    if buf[1] != expect_opcode {
                        last = MapError::Refused(format!(
                            "unexpected NAT-PMP opcode {} (wanted {expect_opcode})",
                            buf[1]
                        ));
                        continue;
                    }
                    let result = u16::from_be_bytes([buf[2], buf[3]]);
                    if result != 0 {
                        last = match result {
                            1 => MapError::Refused("NAT-PMP unsupported version".into()),
                            2 => MapError::Refused("NAT-PMP not authorized".into()),
                            3 => MapError::Io("NAT-PMP network failure".into()),
                            4 => MapError::Refused("NAT-PMP out of resources".into()),
                            5 => MapError::Refused("NAT-PMP unsupported opcode".into()),
                            other => MapError::Refused(format!("NAT-PMP result code {other}")),
                        };
                        continue;
                    }
                    return Ok(buf[..n].to_vec());
                }
                Ok(_) => last = MapError::Io("short NAT-PMP response".into()),
                Err(e) => last = MapError::Io(e.to_string()),
            }
        }
        Err(last)
    }
}

impl PortMapper for NatPmpMapper {
    fn method(&self) -> MapMethod {
        MapMethod::NatPmp
    }

    fn gateway(&self) -> IpAddr {
        self.gateway
    }

    fn wan_ip(&self) -> Result<IpAddr, MapError> {
        // Request: version 0, opcode 0. Response: 8 bytes, IPv4 at [4..8].
        let resp = self.request(&[0, 0], 128)?;
        Ok(IpAddr::V4(Ipv4Addr::new(
            resp[4], resp[5], resp[6], resp[7],
        )))
    }

    fn add_tcp(
        &self,
        internal: SocketAddr,
        external_port: u16,
        lease: Duration,
    ) -> Result<u16, MapError> {
        // Request: v0, opcode 1(TCP), reserved u16, internal port u16,
        // suggested external port u16, lifetime u32.
        let mut req = [0u8; 12];
        req[1] = 1;
        req[4..6].copy_from_slice(&internal.port().to_be_bytes());
        req[6..8].copy_from_slice(&external_port.to_be_bytes());
        req[8..12].copy_from_slice(&(lease.as_secs().min(u32::MAX as u64) as u32).to_be_bytes());
        let resp = self.request(&req, 129)?;
        // Response: result u16 [2..4], epoch u32 [4..8], internal u16 [8..10],
        // mapped external port u16 [10..12], lifetime u32 [12..16].
        if resp.len() < 12 {
            return Err(MapError::Io("short NAT-PMP mapping response".into()));
        }
        let mapped = u16::from_be_bytes([resp[10], resp[11]]);
        if mapped == 0 {
            return Err(MapError::Refused("NAT-PMP mapped external port 0".into()));
        }
        Ok(mapped)
    }

    fn delete_tcp(&self, external_port: u16) -> Result<(), MapError> {
        // Lifetime 0 with the same ports removes the mapping (RFC 6886 §3.4).
        let mut req = [0u8; 12];
        req[1] = 1;
        req[6..8].copy_from_slice(&external_port.to_be_bytes());
        let _ = self.request(&req, 129)?;
        Ok(())
    }
}

/// Best-effort default IPv4 gateway detection.
pub fn default_gateway_v4() -> Option<IpAddr> {
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string("/proc/net/route").ok()?;
        for line in text.lines().skip(1) {
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.len() < 3 || cols[1] != "00000000" {
                continue;
            }
            // Gateway is little-endian hex.
            let raw = u32::from_str_radix(cols[2], 16).ok()?;
            let ip = Ipv4Addr::from(u32::from_be(raw.swap_bytes()));
            if !ip.is_unspecified() {
                return Some(IpAddr::V4(ip));
            }
        }
        None
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("route")
            .args(["-n", "get", "default"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if let Some(rest) = line.trim().strip_prefix("gateway:") {
                return rest.trim().parse::<IpAddr>().ok();
            }
        }
        None
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        // Windows/other: no portable std API. GPN_NATPMP_GATEWAY covers it.
        None
    }
}

// ── the ladder ───────────────────────────────────────────────────────────

/// Configuration for the mapping ladder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortMapConfig {
    /// `GPN_UPNP=auto|off` (and `--no-upnp`). Default `true`.
    pub enabled: bool,
    /// The TCP port to open (external and internal by default).
    pub port: u16,
    /// Lease duration; renewed at half-life.
    pub lease: Duration,
    /// How often to re-run discovery while no mapping is live
    /// (`GPN_UPNP_RETRY_SECS`, default [`RECOVERY_SECS`], floor 30 s).
    pub retry: Duration,
    /// `GPN_NATPMP_GATEWAY` — skip OS gateway detection.
    pub gateway_override: Option<IpAddr>,
}

impl PortMapConfig {
    /// Resolve from the environment. `fallback_port` is the node's listen port.
    pub fn from_env(fallback_port: Option<u16>) -> Self {
        let enabled = parse_upnp(std::env::var("GPN_UPNP").ok().as_deref());
        let port = std::env::var("GPN_SEED_PORT")
            .ok()
            .and_then(|s| s.trim().parse::<u16>().ok())
            .filter(|p| *p != 0)
            .or(fallback_port)
            .unwrap_or(crate::dialability::DEFAULT_SEED_PORT);
        let lease = std::env::var("GPN_UPNP_LEASE_SECS")
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .map(|s| s.max(60))
            .unwrap_or(DEFAULT_LEASE_SECS);
        let gateway_override = std::env::var("GPN_NATPMP_GATEWAY")
            .ok()
            .and_then(|s| s.trim().parse::<IpAddr>().ok());
        let retry = Duration::from_secs(parse_retry_secs(
            std::env::var("GPN_UPNP_RETRY_SECS").ok().as_deref(),
        ));
        PortMapConfig {
            enabled,
            port,
            lease: Duration::from_secs(lease as u64),
            retry,
            gateway_override,
        }
    }
}

/// Parse `GPN_UPNP_RETRY_SECS`. Default [`RECOVERY_SECS`]; a too-small value is
/// floored at 30 s so a typo cannot hammer the router, and a huge one is capped
/// at one hour so re-detection never effectively stops.
pub fn parse_retry_secs(raw: Option<&str>) -> u64 {
    raw.and_then(|s| s.trim().parse::<u64>().ok())
        .map(|s| s.clamp(30, 3600))
        .unwrap_or(RECOVERY_SECS)
}

/// Parse `GPN_UPNP`. Default **on** (`auto`); only an explicit off opts out.
pub fn parse_upnp(raw: Option<&str>) -> bool {
    !matches!(
        raw.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        Some("off" | "0" | "false" | "no" | "disabled")
    )
}

/// Which port the router should forward to on our side (the listen port).
pub fn internal_addr(lan: IpAddr, port: u16) -> SocketAddr {
    SocketAddr::new(lan, port)
}

/// The address to advertise, given the top override and the mapping.
///
/// * `GPN_PUBLIC_ADDR` always wins (an operator may have a port-rewriting
///   forward that we cannot discover).
/// * Otherwise a successful mapping's **external IP + external port** — which
///   is exactly right when the router assigns a different external port.
/// * Otherwise nothing (the gate falls back to observation).
pub fn advertised_address(
    configured: Option<&Multiaddr>,
    mapping: Option<&Mapping>,
) -> Option<Multiaddr> {
    if let Some(ma) = configured {
        return Some(ma.clone());
    }
    let m = mapping?;
    let mut out = Multiaddr::empty();
    match m.external.ip() {
        IpAddr::V4(v4) => out.push(Protocol::Ip4(v4)),
        IpAddr::V6(v6) => out.push(Protocol::Ip6(v6)),
    }
    out.push(Protocol::Tcp(m.external.port()));
    Some(out)
}

/// Run the ladder against a list of candidate mappers (UPnP first, NAT-PMP
/// second). Synchronous and transport-agnostic so it is unit-testable with
/// mock mappers.
///
/// Order per backend: connect → WAN IP → CGNAT/private check → add mapping.
/// A CGNAT/private WAN short-circuits the whole ladder: no later backend can
/// beat it.
pub fn ladder(mappers: &[&dyn PortMapper], lan: IpAddr, port: u16, lease: Duration) -> MapOutcome {
    let internal = internal_addr(lan, port);
    let mut reasons: Vec<String> = Vec::new();
    for m in mappers {
        let method = m.method();
        let wan = match m.wan_ip() {
            Ok(ip) => ip,
            Err(e) => {
                log::debug!(
                    "portmap: {method} at {} could not report its WAN address ({e})",
                    m.gateway()
                );
                reasons.push(format!("{method}: {e}"));
                continue;
            }
        };
        match classify_wan(&wan) {
            WanClass::Cgnat => {
                log::warn!(
                    "portmap: router WAN address {wan} is carrier-grade NAT (100.64.0.0/10) \
                     reported by {method}; no port forward can work"
                );
                return MapOutcome::Cgnat { wan, method };
            }
            WanClass::Private => {
                log::warn!(
                    "portmap: router WAN address {wan} is private (double NAT) reported by \
                     {method}; no port forward can work"
                );
                return MapOutcome::Cgnat { wan, method };
            }
            WanClass::Unusable => {
                reasons.push(format!("{method}: WAN address {wan} is not usable"));
                continue;
            }
            WanClass::Public => {}
        }
        match m.add_tcp(internal, port, lease) {
            Ok(external_port) => {
                let mapping = Mapping {
                    internal,
                    external: SocketAddr::new(wan, external_port),
                    method,
                    lease,
                    gateway: m.gateway(),
                };
                return MapOutcome::Mapped(mapping);
            }
            Err(e) => {
                log::debug!("portmap: {method} at {} failed to map: {e}", m.gateway());
                reasons.push(format!("{method}: {e}"));
            }
        }
    }
    MapOutcome::Failed {
        reason: if reasons.is_empty() {
            "no UPnP-IGD or NAT-PMP gateway found".to_string()
        } else {
            reasons.join("; ")
        },
    }
}

/// The result of starting the mapping ladder, plus everything needed to keep
/// the rule alive.
#[derive(Clone)]
pub struct MappingSession {
    /// What happened (drives the gate and the notice).
    pub outcome: MapOutcome,
    /// The backend that owns the live rule (for [`run_maintenance`]).
    pub mapper: Option<Arc<dyn PortMapper>>,
    /// The config used (port + lease), for renewal.
    pub config: PortMapConfig,
    /// This host's LAN address — the mapping target, reused by every
    /// re-detection attempt in [`run_maintenance`].
    pub lan: IpAddr,
}

impl MappingSession {
    /// The successful mapping, if any.
    pub fn mapping(&self) -> Option<&Mapping> {
        match &self.outcome {
            MapOutcome::Mapped(m) => Some(m),
            _ => None,
        }
    }
}

/// Build the backend list and run [`ladder`] off the async runtime.
///
/// Returns the outcome plus the backend that owns the rule (if any), so the
/// caller can spawn [`run_maintenance`].
pub async fn start(cfg: &PortMapConfig, lan: IpAddr) -> MappingSession {
    let (outcome, mapper) = attempt(cfg, lan).await;
    MappingSession {
        outcome,
        mapper,
        config: cfg.clone(),
        lan,
    }
}

/// Build the backend list and run [`ladder`] off the async runtime (announcing
/// the attempt: the startup path's consent record).
pub async fn attempt(
    cfg: &PortMapConfig,
    lan: IpAddr,
) -> (MapOutcome, Option<Arc<dyn PortMapper>>) {
    attempt_with(cfg, lan, true).await
}

/// [`attempt`], with the consent-record log lines suppressed when this is a
/// silent re-detection retry (`announce = false`). The outcome still reaches
/// the operator through [`maintenance_notice`] — on **change** only.
async fn attempt_with(
    cfg: &PortMapConfig,
    lan: IpAddr,
    announce: bool,
) -> (MapOutcome, Option<Arc<dyn PortMapper>>) {
    if !cfg.enabled {
        log::info!(
            "portmap: UPnP/NAT-PMP DISABLED (GPN_UPNP=off / --no-upnp) - the router will not \
             be contacted; TCP {} must be forwarded manually or via GPN_PUBLIC_ADDR",
            cfg.port
        );
        return (MapOutcome::Disabled, None);
    }

    if announce {
        log::info!(
            "portmap: attempting automatic mapping of TCP {} -> {lan}:{} via UPnP-IGD, then \
             NAT-PMP (lease {}s; disable with GPN_UPNP=off or --no-upnp)",
            cfg.port,
            cfg.port,
            cfg.lease.as_secs()
        );
    } else {
        log::debug!(
            "portmap: re-detection attempt for TCP {} -> {lan}:{}",
            cfg.port,
            cfg.port
        );
    }

    let cfg = cfg.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        let igd = IgdMapper::discover();
        let igd_err = igd.as_ref().err().map(|e| e.to_string());

        let natpmp_gw = cfg
            .gateway_override
            .or_else(|| igd.as_ref().ok().map(|g| g.gateway()))
            .or_else(|| NatPmpMapper::discover().map(|m| m.gateway));
        let natpmp = natpmp_gw.map(NatPmpMapper::new);

        if let Some(e) = igd_err {
            log::debug!("portmap: UPnP-IGD discovery failed ({e}); trying NAT-PMP");
        }

        let mut mappers: Vec<Arc<dyn PortMapper>> = Vec::new();
        if let Ok(g) = igd {
            mappers.push(Arc::new(g));
        }
        if let Some(n) = natpmp {
            mappers.push(Arc::new(n));
        }
        let refs: Vec<&dyn PortMapper> = mappers.iter().map(|b| b.as_ref()).collect();
        let outcome = ladder(&refs, lan, cfg.port, cfg.lease);
        let owner = match &outcome {
            MapOutcome::Mapped(m) => mappers.iter().find(|c| c.method() == m.method).cloned(),
            _ => None,
        };
        (outcome, owner)
    })
    .await;

    match outcome {
        Ok(o) => o,
        Err(e) => (
            MapOutcome::Failed {
                reason: format!("port mapping task failed: {e}"),
            },
            None,
        ),
    }
}

/// Log one line per outcome — the consent record.
pub fn log_outcome(outcome: &MapOutcome, port: u16) {
    match outcome {
        MapOutcome::Mapped(m) => {
            log::info!(
                "portmap: UPnP: mapped TCP {port} -> {} via {} ({}); external address is {}. \
                 Disable with GPN_UPNP=off or --no-upnp",
                m.internal,
                m.gateway,
                m.method,
                m.external
            );
            if m.external.port() != m.internal.port() {
                log::info!(
                    "portmap: router assigned external port {} (internal is {}); the advertised \
                     address uses the external port",
                    m.external.port(),
                    m.internal.port()
                );
            }
        }
        MapOutcome::Cgnat { wan, method } => {
            log::warn!(
                "portmap: {method} reports WAN address {wan}; this is CGNAT or double NAT, so no \
                 port forward can make this node dialable"
            );
        }
        MapOutcome::Disabled => {
            log::info!(
                "portmap: automatic mapping is OFF (GPN_UPNP=off / --no-upnp); TCP {port} must be \
                 forwarded manually or covered by GPN_PUBLIC_ADDR"
            );
        }
        MapOutcome::Failed { reason } => {
            log::warn!(
                "portmap: automatic port mapping FAILED ({reason}); TCP {port} must be forwarded \
                 manually. If your router has UPnP disabled, enabling it - or adding the forward \
                 - is the fix."
            );
        }
    }
}

/// The dialability-relevant state of the mapping ladder. Used for
/// **edge-triggered** logging: a retry that fails the same way again is silent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapState {
    /// A rule is live; this is the internet-side address the router reported.
    Mapped(SocketAddr),
    /// The router's WAN address is CGNAT/private: no forward can ever work.
    Cgnat(IpAddr),
    /// No usable rule (no gateway, refused, or mapping disabled).
    Unavailable,
}

/// Collapse a ladder outcome to the state we log against.
pub fn state_of(outcome: &MapOutcome) -> MapState {
    match outcome {
        MapOutcome::Mapped(m) => MapState::Mapped(m.external),
        MapOutcome::Cgnat { wan, .. } => MapState::Cgnat(*wan),
        MapOutcome::Failed { .. } | MapOutcome::Disabled => MapState::Unavailable,
    }
}

/// One line to log when the mapping state changes, and how loudly.
pub struct MaintenanceNotice {
    /// `true` → `warn`, `false` → `info`.
    pub warn: bool,
    /// The line, already log-safe and ASCII.
    pub line: String,
}

/// Decide what to say when the mapping state changes (design §5.3:
/// edge-triggered, never nag). `None` means "say nothing".
pub fn maintenance_notice(
    prev: &MapState,
    now: &MapState,
    port: u16,
    retry: Duration,
) -> Option<MaintenanceNotice> {
    if prev == now {
        return None;
    }
    let every = retry.as_secs();
    Some(match now {
        MapState::Mapped(addr) => MaintenanceNotice {
            warn: false,
            line: match prev {
                MapState::Cgnat(wan) => format!(
                    "portmap: the router's WAN address is public again ({wan} -> {addr}); TCP \
                     {port} is now mapped. The dialability watcher will confirm inbound \
                     reachability; restart the node to advertise the new address."
                ),
                MapState::Mapped(old) => format!(
                    "portmap: the mapped external address CHANGED {old} -> {addr}; the rule was \
                     renewed with the new address. libp2p's announced address set is fixed at \
                     startup - restart the node to advertise it."
                ),
                _ => format!(
                    "portmap: automatic mapping is ACTIVE again on TCP {port} -> {addr} \
                     (UPnP-IGD/NAT-PMP re-established). The dialability watcher will confirm \
                     inbound reachability; restart the node to advertise the new address."
                ),
            },
        },
        MapState::Cgnat(wan) => MaintenanceNotice {
            warn: true,
            line: format!(
                "portmap: the router's WAN address is {wan} (CGNAT or double NAT) - no port \
                 forward can make TCP {port} dialable. Re-checking every {every}s in case the \
                 ISP changes it; GPN_UPNP=off silences this."
            ),
        },
        MapState::Unavailable => MaintenanceNotice {
            warn: true,
            line: format!(
                "portmap: no automatic mapping for TCP {port} (no UPnP-IGD/NAT-PMP gateway, or \
                 the router refused). Still trying every {every}s - enable UPnP on the router, \
                 or forward the port manually; GPN_UPNP=off silences this."
            ),
        },
    })
}

/// Re-add (refresh) one existing rule through the backend that owns it.
async fn renew_once(
    mapper: &Arc<dyn PortMapper>,
    current: &Mapping,
    port: u16,
) -> Result<Mapping, String> {
    let m = mapper.clone();
    let internal = current.internal;
    let lease = current.lease;
    tokio::task::spawn_blocking(move || -> Result<Mapping, String> {
        let wan = m.wan_ip().map_err(|e| e.to_string())?;
        match classify_wan(&wan) {
            WanClass::Public => {}
            other => {
                return Err(format!(
                    "WAN address {wan} is no longer public ({other:?}) - CGNAT change?"
                ))
            }
        }
        let external_port = m
            .add_tcp(internal, port, lease)
            .map_err(|e| e.to_string())?;
        Ok(Mapping {
            internal,
            external: SocketAddr::new(wan, external_port),
            method: m.method(),
            lease,
            gateway: m.gateway(),
        })
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()))
}

/// Keep the rule alive **and** re-detect a router when there is none.
///
/// Two cadences, one loop:
///
/// * **live mapping** → renew at half the lease (default every 30 min). A
///   renewal that fails drops the stale backend and immediately re-runs full
///   discovery — a router reboot or WAN change no longer needs a node restart
///   to get the port back.
/// * **no mapping** → re-run the whole UPnP-IGD → NAT-PMP ladder every
///   `cfg.retry` (default 10 min). This is what makes "enable UPnP on the
///   router" work without restarting the node, and it also picks up a WAN
///   address that moved off CGNAT.
///
/// Logging is edge-triggered via [`maintenance_notice`], so a permanently
/// non-mappable node says so once, not every 10 minutes.
pub async fn run_maintenance(initial: MappingSession) {
    let cfg = initial.config.clone();
    if !cfg.enabled {
        return;
    }
    let lan = initial.lan;
    let mut current: Option<Mapping> = initial.mapping().cloned();
    let mut mapper: Option<Arc<dyn PortMapper>> = initial.mapper.clone();
    let mut last = state_of(&initial.outcome);

    loop {
        let delay = match &current {
            Some(m) => (m.lease / 2).max(Duration::from_secs(30)),
            None => cfg.retry,
        };
        tokio::time::sleep(delay).await;

        let outcome = match (current.clone(), mapper.clone()) {
            (Some(m), Some(mp)) => match renew_once(&mp, &m, cfg.port).await {
                Ok(fresh) => {
                    log::debug!(
                        "portmap: renewed {} mapping for TCP {} (external {})",
                        fresh.method,
                        cfg.port,
                        fresh.external
                    );
                    MapOutcome::Mapped(fresh)
                }
                Err(reason) => {
                    log::debug!(
                        "portmap: renewal of TCP {} failed ({reason}); re-running discovery",
                        cfg.port
                    );
                    let (o, mp2) = attempt_with(&cfg, lan, false).await;
                    mapper = mp2;
                    o
                }
            },
            _ => {
                let (o, mp2) = attempt_with(&cfg, lan, false).await;
                mapper = mp2;
                o
            }
        };

        current = match &outcome {
            MapOutcome::Mapped(m) => Some(m.clone()),
            _ => None,
        };
        if current.is_none() {
            mapper = None;
        }

        let now = state_of(&outcome);
        if let Some(n) = maintenance_notice(&last, &now, cfg.port, cfg.retry) {
            if n.warn {
                log::warn!("{}", n.line);
            } else {
                log::info!("{}", n.line);
            }
        }
        if let MapOutcome::Mapped(m) = &outcome {
            if m.external.port() != m.internal.port() {
                log::debug!(
                    "portmap: router keeps external port {} for internal {}",
                    m.external.port(),
                    m.internal.port()
                );
            }
        }
        last = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn lan() -> IpAddr {
        ip("192.168.1.42")
    }

    // ── CGNAT / WAN classification ────────────────────────────────────────

    #[test]
    fn cgnat_range_is_exact() {
        assert!(is_cgnat(&ip("100.64.0.0")));
        assert!(is_cgnat(&ip("100.64.7.9")));
        assert!(is_cgnat(&ip("100.127.255.255")));
        // Just outside /10 on both sides.
        assert!(!is_cgnat(&ip("100.63.255.255")));
        assert!(!is_cgnat(&ip("100.128.0.0")));
        assert!(!is_cgnat(&ip("10.0.0.1")));
        assert!(!is_cgnat(&ip("8.8.8.8")));
        assert!(!is_cgnat(&ip("2001:db8::1")));
    }

    #[test]
    fn wan_classification_distinguishes_cgnat_from_double_nat() {
        assert_eq!(classify_wan(&ip("8.8.8.8")), WanClass::Public);
        assert_eq!(classify_wan(&ip("100.64.1.1")), WanClass::Cgnat);
        assert_eq!(classify_wan(&ip("10.1.2.3")), WanClass::Private);
        assert_eq!(classify_wan(&ip("192.168.1.1")), WanClass::Private);
        assert_eq!(classify_wan(&ip("127.0.0.1")), WanClass::Unusable);
        assert_eq!(classify_wan(&ip("0.0.0.0")), WanClass::Unusable);
        assert_eq!(classify_wan(&ip("fd00::1")), WanClass::Private);
    }

    #[test]
    fn parse_upnp_defaults_auto_and_opts_out() {
        assert!(parse_upnp(None));
        assert!(parse_upnp(Some("auto")));
        assert!(parse_upnp(Some("")));
        assert!(parse_upnp(Some("1")));
        for v in ["off", "OFF", "0", "false", "no", "disabled", " off "] {
            assert!(!parse_upnp(Some(v)), "{v} must disable UPnP");
        }
    }

    // ── mock mappers ──────────────────────────────────────────────────────

    struct MockMapper {
        method: MapMethod,
        gateway: IpAddr,
        wan: Result<IpAddr, MapError>,
        // external port the "router" assigns
        assigned: u16,
        add_err: Option<MapError>,
        added: Mutex<Vec<(SocketAddr, u16, u32)>>,
    }

    impl MockMapper {
        fn upnp(wan: &str) -> Self {
            MockMapper {
                method: MapMethod::UpnpIgd,
                gateway: ip("192.168.1.1"),
                wan: Ok(ip(wan)),
                assigned: 30333,
                add_err: None,
                added: Mutex::new(Vec::new()),
            }
        }
        fn with_assigned(mut self, p: u16) -> Self {
            self.assigned = p;
            self
        }
        fn failing(mut self, e: MapError) -> Self {
            self.add_err = Some(e);
            self
        }
    }

    impl PortMapper for MockMapper {
        fn method(&self) -> MapMethod {
            self.method
        }
        fn gateway(&self) -> IpAddr {
            self.gateway
        }
        fn wan_ip(&self) -> Result<IpAddr, MapError> {
            self.wan.clone()
        }
        fn add_tcp(
            &self,
            internal: SocketAddr,
            external_port: u16,
            lease: Duration,
        ) -> Result<u16, MapError> {
            self.added
                .lock()
                .unwrap()
                .push((internal, external_port, lease.as_secs() as u32));
            match &self.add_err {
                Some(e) => Err(e.clone()),
                None => Ok(self.assigned),
            }
        }
        fn delete_tcp(&self, _external_port: u16) -> Result<(), MapError> {
            Ok(())
        }
    }

    // ── ladder ────────────────────────────────────────────────────────────

    #[test]
    fn ladder_maps_with_upnp_and_uses_internal_listen_port() {
        let m = MockMapper::upnp("23.241.109.255");
        let out = ladder(&[&m], lan(), 30333, Duration::from_secs(3600));
        let mapping = match out {
            MapOutcome::Mapped(m) => m,
            other => panic!("expected mapping, got {other:?}"),
        };
        assert_eq!(mapping.internal, "192.168.1.42:30333".parse().unwrap());
        assert_eq!(mapping.external, "23.241.109.255:30333".parse().unwrap());
        assert_eq!(mapping.method, MapMethod::UpnpIgd);
        assert_eq!(mapping.gateway, ip("192.168.1.1"));
        let calls = m.added.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1, 30333, "requests the port-preserving mapping");
        assert_eq!(calls[0].2, 3600, "lease is forwarded to the router");
    }

    #[test]
    fn external_port_different_from_internal_is_advertised() {
        // The router gave us 45123 -> our 30333.
        let m = MockMapper::upnp("23.241.109.255").with_assigned(45123);
        let out = ladder(&[&m], lan(), 30333, Duration::from_secs(3600));
        let mapping = match out {
            MapOutcome::Mapped(m) => m,
            other => panic!("expected mapping, got {other:?}"),
        };
        let adv = advertised_address(None, Some(&mapping)).expect("mapping advertises");
        assert_eq!(adv.to_string(), "/ip4/23.241.109.255/tcp/45123");

        // And the gate's endpoint parser must read the *external* port, not the
        // internal one, so an observation of that address can confirm it.
        let (aip, aport) = crate::dialability::endpoint_of(&adv).unwrap();
        assert_eq!(aip, ip("23.241.109.255"));
        assert_eq!(aport, 45123);
    }

    #[test]
    fn advertised_address_prefers_configured_override() {
        let cfg: Multiaddr = "/ip4/1.2.3.4/tcp/40000".parse().unwrap();
        let m = MockMapper::upnp("23.241.109.255");
        let mapping = match ladder(&[&m], lan(), 30333, Duration::from_secs(60)) {
            MapOutcome::Mapped(m) => m,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            advertised_address(Some(&cfg), Some(&mapping)).unwrap(),
            cfg,
            "GPN_PUBLIC_ADDR is the top override"
        );
        assert!(advertised_address(None, None).is_none());
    }

    #[test]
    fn ladder_short_circuits_on_cgnat() {
        let upnp = MockMapper::upnp("100.64.9.9");
        let natpmp = MockMapper {
            method: MapMethod::NatPmp,
            gateway: ip("192.168.1.1"),
            wan: Ok(ip("23.241.109.255")),
            assigned: 30333,
            add_err: None,
            added: Mutex::new(Vec::new()),
        };
        let out = ladder(&[&upnp, &natpmp], lan(), 30333, Duration::from_secs(60));
        assert_eq!(
            out,
            MapOutcome::Cgnat {
                wan: ip("100.64.9.9"),
                method: MapMethod::UpnpIgd
            }
        );
        // No mapping was attempted anywhere.
        assert!(upnp.added.lock().unwrap().is_empty());
        assert!(natpmp.added.lock().unwrap().is_empty());
    }

    #[test]
    fn ladder_short_circuits_on_private_wan_as_cgnat() {
        let m = MockMapper::upnp("10.0.0.2");
        let out = ladder(&[&m], lan(), 30333, Duration::from_secs(60));
        assert!(matches!(out, MapOutcome::Cgnat { .. }), "{out:?}");
        assert!(m.added.lock().unwrap().is_empty());
    }

    #[test]
    fn ladder_falls_back_to_natpmp_when_upnp_fails() {
        let upnp = MockMapper::upnp("23.241.109.255").failing(MapError::Refused("no IGD".into()));
        let natpmp = MockMapper {
            method: MapMethod::NatPmp,
            gateway: ip("192.168.1.1"),
            wan: Ok(ip("23.241.109.255")),
            assigned: 30333,
            add_err: None,
            added: Mutex::new(Vec::new()),
        };
        let out = ladder(&[&upnp, &natpmp], lan(), 30333, Duration::from_secs(60));
        match out {
            MapOutcome::Mapped(m) => assert_eq!(m.method, MapMethod::NatPmp),
            other => panic!("expected NAT-PMP mapping, got {other:?}"),
        }
        assert_eq!(natpmp.added.lock().unwrap().len(), 1);
    }

    #[test]
    fn ladder_reports_all_failures() {
        let a = MockMapper {
            method: MapMethod::UpnpIgd,
            gateway: ip("192.168.1.1"),
            wan: Err(MapError::NoGateway("ssdp silent".into())),
            assigned: 0,
            add_err: None,
            added: Mutex::new(Vec::new()),
        };
        let out = ladder(&[&a], lan(), 30333, Duration::from_secs(60));
        match out {
            MapOutcome::Failed { reason } => {
                assert!(reason.contains("UPnP-IGD"), "{reason}");
                assert!(reason.contains("ssdp silent"), "{reason}");
            }
            other => panic!("expected failure, got {other:?}"),
        }
        // Empty ladder must still be a usable message.
        let empty = ladder(&[], lan(), 30333, Duration::from_secs(60));
        assert!(matches!(empty, MapOutcome::Failed { .. }));
    }

    // ── renewal ───────────────────────────────────────────────────────────

    fn mapping(ext: &str) -> Mapping {
        Mapping {
            internal: "192.168.1.42:30333".parse().unwrap(),
            external: ext.parse().unwrap(),
            method: MapMethod::UpnpIgd,
            lease: Duration::from_secs(3600),
            gateway: ip("192.168.1.1"),
        }
    }

    // ── maintenance: edge-triggered notices + retry cadence ──────────────

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn mapping_states_collapse_outcomes() {
        let m = mapping("23.241.109.255:30333");
        assert_eq!(
            state_of(&MapOutcome::Mapped(m.clone())),
            MapState::Mapped(addr("23.241.109.255:30333"))
        );
        assert_eq!(
            state_of(&MapOutcome::Cgnat {
                wan: ip("100.64.1.1"),
                method: MapMethod::UpnpIgd
            }),
            MapState::Cgnat(ip("100.64.1.1"))
        );
        assert_eq!(
            state_of(&MapOutcome::Failed {
                reason: "no gateway".into()
            }),
            MapState::Unavailable
        );
        assert_eq!(state_of(&MapOutcome::Disabled), MapState::Unavailable);
    }

    #[test]
    fn maintenance_is_edge_triggered_and_never_nags() {
        let retry = Duration::from_secs(RECOVERY_SECS);
        let mapped = MapState::Mapped(addr("23.241.109.255:30333"));

        // Unchanged state says nothing, whatever it is.
        assert!(maintenance_notice(&mapped, &mapped, 30333, retry).is_none());
        assert!(
            maintenance_notice(&MapState::Unavailable, &MapState::Unavailable, 30333, retry)
                .is_none()
        );
        assert!(maintenance_notice(
            &MapState::Cgnat(ip("100.64.1.1")),
            &MapState::Cgnat(ip("100.64.1.1")),
            30333,
            retry
        )
        .is_none());

        // Losing the rule warns once, and says it keeps trying.
        let lost = maintenance_notice(&mapped, &MapState::Unavailable, 30333, retry).unwrap();
        assert!(lost.warn);
        assert!(lost.line.contains("no automatic mapping"), "{}", lost.line);
        assert!(lost.line.contains("600s"), "{}", lost.line);
        assert!(lost.line.is_ascii());

        // Regaining it is good news, and points at the restart-to-advertise rule.
        let back = maintenance_notice(&MapState::Unavailable, &mapped, 30333, retry).unwrap();
        assert!(!back.warn);
        assert!(back.line.contains("ACTIVE again"), "{}", back.line);
        assert!(back.line.contains("restart"), "{}", back.line);

        // A WAN/port change is explicitly called out, not reported as a recovery.
        let moved = MapState::Mapped(addr("23.241.109.254:45123"));
        let changed = maintenance_notice(&mapped, &moved, 30333, retry).unwrap();
        assert!(!changed.warn);
        assert!(changed.line.contains("CHANGED"), "{}", changed.line);
        assert!(changed.line.contains("45123"), "{}", changed.line);

        // Falling into CGNAT warns, and is recoverable when the WAN moves again.
        let cgnat =
            maintenance_notice(&mapped, &MapState::Cgnat(ip("100.64.9.9")), 30333, retry).unwrap();
        assert!(cgnat.warn);
        assert!(cgnat.line.contains("CGNAT"), "{}", cgnat.line);
        let recovered =
            maintenance_notice(&MapState::Cgnat(ip("100.64.9.9")), &mapped, 30333, retry).unwrap();
        assert!(!recovered.warn);
        assert!(
            recovered.line.contains("public again"),
            "{}",
            recovered.line
        );
    }

    #[test]
    fn retry_cadence_defaults_to_ten_minutes_and_is_bounded() {
        assert_eq!(parse_retry_secs(None), RECOVERY_SECS);
        assert_eq!(RECOVERY_SECS, 600);
        assert_eq!(parse_retry_secs(Some("600")), 600);
        assert_eq!(parse_retry_secs(Some(" 90 ")), 90);
        // A typo cannot hammer the router every second...
        assert_eq!(parse_retry_secs(Some("1")), 30);
        assert_eq!(parse_retry_secs(Some("0")), 30);
        // ...and re-detection never effectively stops.
        assert_eq!(parse_retry_secs(Some("999999")), 3600);
        assert_eq!(parse_retry_secs(Some("nonsense")), RECOVERY_SECS);
    }

    #[test]
    fn a_failed_ladder_is_retried_not_fatal() {
        // Two backends that cannot reach a router: the ladder reports a failure
        // (so `run_maintenance` keeps looping) instead of returning an error
        // that would end the task and leave the node permanently unmapped.
        let dead = MockMapper {
            method: MapMethod::NatPmp,
            gateway: ip("192.168.1.1"),
            wan: Err(MapError::NoGateway("udp/5351 silent".into())),
            assigned: 0,
            add_err: None,
            added: Mutex::new(Vec::new()),
        };
        let out = ladder(&[&dead], lan(), 30333, Duration::from_secs(60));
        assert!(matches!(out, MapOutcome::Failed { .. }), "{out:?}");
        assert_eq!(state_of(&out), MapState::Unavailable);

        // And the session carries the LAN address the retry loop targets.
        let session = MappingSession {
            outcome: out,
            mapper: None,
            config: PortMapConfig {
                enabled: true,
                port: 30333,
                lease: Duration::from_secs(3600),
                retry: Duration::from_secs(RECOVERY_SECS),
                gateway_override: None,
            },
            lan: lan(),
        };
        assert!(session.mapping().is_none());
        assert_eq!(session.lan, lan());
        assert_eq!(session.config.retry, Duration::from_secs(RECOVERY_SECS));
    }

    // ── NAT-PMP wire format (no network: encodes requests only) ───────────

    #[test]
    fn natpmp_mapper_is_tcp_only_and_reports_its_gateway() {
        let m = NatPmpMapper::new(ip("192.168.1.1"));
        assert_eq!(m.method(), MapMethod::NatPmp);
        assert_eq!(m.gateway(), ip("192.168.1.1"));
    }

    #[test]
    fn internal_addr_uses_listen_port() {
        assert_eq!(
            internal_addr(lan(), 30333),
            "192.168.1.42:30333".parse().unwrap()
        );
    }
}
