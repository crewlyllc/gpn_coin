//! Same-LAN peer discovery (queue row D3).
//!
//! ## Decision: mDNS, not a shipped LAN seed
//!
//! A same-LAN pair must not have to leave the subnet and come back through the
//! router to find each other (hairpin NAT), and a private seed address must
//! never be *shipped*: it is only meaningful on the /24 that owns it, which is
//! exactly the mistake `LAN_SEED_BOOTNODE` made in 0.3.29 (see
//! `gpn-agent/src/chainspec.rs`). mDNS is the mechanism that costs no
//! configuration and cannot point a stranger at somebody else's device.
//!
//! ## What this module exists for
//!
//! Enabling mDNS is necessary but **not sufficient**, and the failure is silent.
//! In this SDK rev (`sc-network` 0.34 at `da5c966`) the two backends differ:
//!
//! * `litep2p` — `MdnsEvent::Discovered(addresses)` is forwarded as
//!   `DiscoveryEvent::Discovered { addresses }` and each address is inserted
//!   with `litep2p.add_known_address(...)`
//!   (`substrate/client/network/src/litep2p/mod.rs:868-874`). LAN peers become
//!   dialable.
//! * `libp2p` — `libp2p-mdns` 0.46 emits `Event::Discovered(Vec<(PeerId,
//!   Multiaddr)>)` and inserts nothing itself; sc-network's
//!   `DiscoveryBehaviour` throws the addresses away and forwards only
//!   `BehaviourOut::Discovered(peer_id)`, which the worker turns into
//!   `peer_store_handle.add_known_peer(peer_id)`
//!   (`substrate/client/network/src/service.rs:1623`). The peer is "known"
//!   with **zero addresses** and can never be dialled.
//!
//! So `mdns = on` + `--network-backend libp2p` is a configuration that looks
//! enabled, logs nothing, and discovers nothing. That combination has to be
//! visible, so this module states the effective posture once at startup:
//! one INFO line when LAN discovery can work, one WARN when it cannot, naming
//! the backend and the fix.
//!
//! mDNS itself is configured by the substrate CLI (`NetworkParams::network_config`):
//! `enable_mdns = !is_dev && !no_mdns`, `allow_private_ip` = true for
//! `ChainType::Local`/`Development` (which the `testnet` spec is). We do not
//! override the operator's flags; we report what they add up to.

use sc_network::config::{NetworkBackendType, NetworkConfiguration, TransportConfig};

/// The effective same-LAN discovery posture of a node configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Posture {
    /// mDNS is on, the backend forwards the addresses, and private (LAN)
    /// addresses are allowed — discovery can work.
    Active,
    /// mDNS is on, but this backend discards the discovered addresses.
    Inert { backend: &'static str },
    /// mDNS is off (`--dev` or `--no-mdns`): no LAN discovery at all.
    Disabled,
    /// mDNS is on and forwarded, but the dialler refuses private addresses,
    /// which is what a LAN peer always is.
    Filtered,
}

impl Posture {
    /// One line, in the shape the rest of the node's startup log uses.
    pub fn describe(&self) -> String {
        match self {
            Posture::Active => {
                "lan-discovery: mDNS is ACTIVE (litep2p backend, private addresses allowed) — \
                 same-LAN peers are found without touching the router"
                    .to_string()
            }
            Posture::Inert { backend } => format!(
                "lan-discovery: mDNS is ON but INERT on the {backend} backend — this sc-network \
                 rev drops the mDNS (peer, address) pairs and keeps only the peer id, so no LAN \
                 peer ever becomes dialable. Fix: run --network-backend litep2p (the default; \
                 GPN's launchers do not pin a backend)"
            ),
            Posture::Disabled => {
                "lan-discovery: mDNS is OFF (--dev or --no-mdns) — a same-LAN peer can only be \
                 reached through a configured bootnode"
                    .to_string()
            }
            Posture::Filtered => {
                "lan-discovery: mDNS discovers LAN peers but they will be refused — private \
                 addresses are disallowed on this chain (allow_private_ip=false); pass \
                 --allow-private-ip on a private network"
                    .to_string()
            }
        }
    }

    /// `true` when a same-LAN peer can actually be discovered *and* dialled.
    pub fn works(&self) -> bool {
        matches!(self, Posture::Active)
    }
}

/// Classify a node's network configuration. Pure: no sockets, no logging.
pub fn posture(cfg: &NetworkConfiguration) -> Posture {
    let (enable_mdns, allow_private_ip) = match cfg.transport {
        TransportConfig::Normal {
            enable_mdns,
            allow_private_ip,
        } => (enable_mdns, allow_private_ip),
        // `MemoryOnly` is the in-process test transport; it has no LAN.
        TransportConfig::MemoryOnly => return Posture::Disabled,
    };
    if !enable_mdns {
        return Posture::Disabled;
    }
    match cfg.network_backend {
        NetworkBackendType::Libp2p => return Posture::Inert { backend: "libp2p" },
        NetworkBackendType::Litep2p => {}
    }
    if !allow_private_ip {
        return Posture::Filtered;
    }
    Posture::Active
}

/// Log the posture once, before the network is built. WARN when the
/// configuration says "discovery is on" but cannot deliver, so the failure is
/// never silent again.
pub fn log_posture(cfg: &NetworkConfiguration) {
    let p = posture(cfg);
    if p.works() {
        log::info!("{}", p.describe());
    } else {
        log::warn!("{}", p.describe());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sc_network::config::{NetworkBackendType, NetworkConfiguration, TransportConfig};

    fn cfg(backend: NetworkBackendType, mdns: bool, private_ip: bool) -> NetworkConfiguration {
        let mut cfg = NetworkConfiguration::new(
            "test-node",
            "gpn-test/0.0.0",
            sc_network::config::NodeKeyConfig::default(),
            None,
        );
        cfg.network_backend = backend;
        cfg.transport = TransportConfig::Normal {
            enable_mdns: mdns,
            allow_private_ip: private_ip,
        };
        cfg
    }

    /// The regression this module was written for: the seed node runs
    /// `--network-backend libp2p`, where mDNS is enabled and useless.
    #[test]
    fn libp2p_with_mdns_is_reported_inert() {
        let p = posture(&cfg(NetworkBackendType::Libp2p, true, true));
        assert_eq!(p, Posture::Inert { backend: "libp2p" });
        assert!(!p.works());
        let line = p.describe();
        assert!(line.contains("INERT"), "{line}");
        assert!(line.contains("litep2p"), "{line}");
    }

    /// litep2p forwards the discovered addresses, so this is the one shape
    /// that actually delivers LAN discovery.
    #[test]
    fn litep2p_with_mdns_and_private_ip_is_active() {
        let p = posture(&cfg(NetworkBackendType::Litep2p, true, true));
        assert_eq!(p, Posture::Active);
        assert!(p.works());
    }

    /// `--no-mdns` / `--dev` must be reported as off, not as "we tried".
    #[test]
    fn mdns_off_is_disabled_on_every_backend() {
        for backend in [NetworkBackendType::Libp2p, NetworkBackendType::Litep2p] {
            let p = posture(&cfg(backend, false, true));
            assert_eq!(p, Posture::Disabled);
            assert!(!p.works());
        }
    }

    /// A LAN address is private; if the chain forbids private addresses the
    /// discovered peers are filtered at dial time.
    #[test]
    fn litep2p_without_private_ip_is_filtered() {
        let p = posture(&cfg(NetworkBackendType::Litep2p, true, false));
        assert_eq!(p, Posture::Filtered);
        assert!(!p.works());
    }

    /// Exactly one posture "works", so a caller can gate on it.
    #[test]
    fn only_active_posture_works() {
        let cases = [
            (NetworkBackendType::Libp2p, true, true),
            (NetworkBackendType::Libp2p, false, true),
            (NetworkBackendType::Litep2p, true, true),
            (NetworkBackendType::Litep2p, true, false),
            (NetworkBackendType::Litep2p, false, false),
        ];
        let working = cases
            .iter()
            .filter(|(b, m, p)| posture(&cfg(*b, *m, *p)).works())
            .count();
        assert_eq!(working, 1, "only litep2p+mdns+private-ip may claim to work");
    }
}
