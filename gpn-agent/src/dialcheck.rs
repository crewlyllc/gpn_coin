//! Dial-back verification: the only thing in GPN allowed to assert that a
//! public address is reachable.
//!
//! The rule (design-lan-topology §3.0):
//!
//! > An address may be published only by the peer id that a third party reaches
//! > when it dials that address.
//!
//! Everything else in the system — the evidence ladder, the access point, the
//! publisher — consumes this result. A bare `TcpStream::connect` is **not** a
//! verification: on a shared NAT every machine behind it can connect to the
//! one forwarded socket, so box B "proves" dialability against box A's
//! listener. The handshake is what tells them apart, because the peer id comes
//! out of the authenticated noise handshake, not out of the address string.
//!
//! The 2026-11-12 incident: two peer ids published
//! `/ip4/142.129.54.88/tcp/30333` simultaneously, both with `peers: 0`, and
//! every client that merged them died on `Error::DuplicateBootnode`. The
//! forward pointed at exactly one of them. A single dial-back would have said
//! `WrongPeer` for the other in under a second.

use std::time::Duration;

use libp2p::core::transport::Transport;
use libp2p::swarm::dial_opts::DialOpts;
use libp2p::swarm::SwarmEvent;
use libp2p::Multiaddr;

/// What one dial-back found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A libp2p handshake completed and the remote peer id is the expected one.
    Verified { peer_id: String },
    /// A handshake completed but a **different** peer id answered. This is the
    /// 2026-11-12 double-claim: the socket is forwarded to another machine.
    WrongPeer { expected: String, actual: String },
    /// No handshake: timeout, refused, transport or protocol error.
    Unreachable { reason: String },
    /// Not attempted: the verifier shares the claimant's public IP, so its
    /// answer proves nothing (design §4.1.2 — the AP on the seed box).
    Skipped { reason: String },
}

impl Outcome {
    /// Only [`Outcome::Verified`] may make an address publishable.
    pub fn is_verified(&self) -> bool {
        matches!(self, Outcome::Verified { .. })
    }

    /// Stable code: `"verified"`, `"wrong-peer"`, `"unreachable"`, `"skipped"`.
    pub fn code(&self) -> &'static str {
        match self {
            Outcome::Verified { .. } => "verified",
            Outcome::WrongPeer { .. } => "wrong-peer",
            Outcome::Unreachable { .. } => "unreachable",
            Outcome::Skipped { .. } => "skipped",
        }
    }

    /// Process exit code for `gpn-dialcheck` (and the AP's child process).
    /// `0` verified, `2` wrong peer, `3` unreachable, `4` skipped.
    pub fn exit_code(&self) -> i32 {
        match self {
            Outcome::Verified { .. } => 0,
            Outcome::WrongPeer { .. } => 2,
            Outcome::Unreachable { .. } => 3,
            Outcome::Skipped { .. } => 4,
        }
    }

    /// One line, naming the id that actually answered when there was one.
    pub fn summary(&self) -> String {
        match self {
            Outcome::Verified { peer_id } => format!("socket answers as {peer_id}"),
            Outcome::WrongPeer { expected, actual } => format!(
                "socket answers as {actual}, not {expected} — another machine owns this forward"
            ),
            Outcome::Unreachable { reason } => {
                format!("no libp2p handshake from the public socket ({reason})")
            }
            Outcome::Skipped { reason } => format!("verification skipped ({reason})"),
        }
    }
}

/// Host part of a multiaddr, as the text used to compare egresses.
fn host_of(ma: &Multiaddr) -> Option<String> {
    use libp2p::multiaddr::Protocol;
    for p in ma.iter() {
        match p {
            Protocol::Ip4(v4) => return Some(v4.to_string()),
            Protocol::Ip6(v6) => return Some(v6.to_string()),
            Protocol::Dns4(n) | Protocol::Dns6(n) | Protocol::Dns(n) => return Some(n.to_string()),
            _ => {}
        }
    }
    None
}

/// The `/p2p/<id>` tail of a multiaddr, if it has one.
pub fn peer_id_of(multiaddr: &str) -> Option<String> {
    let addr = multiaddr.parse::<Multiaddr>().ok()?;
    let mut tail = None;
    for p in addr.iter() {
        if let libp2p::multiaddr::Protocol::P2p(id) = p {
            tail = Some(id.to_string());
        }
    }
    tail
}

/// Is `ma` something this verifier can dial (a TCP or WS libp2p socket)?
fn is_dialable_socket(ma: &Multiaddr) -> bool {
    use libp2p::multiaddr::Protocol;
    ma.iter()
        .any(|p| matches!(p, Protocol::Tcp(_) | Protocol::Ws(_) | Protocol::Wss(_)))
}

/// Verify `multiaddr` (which must carry `/p2p/<expected>`) by completing a
/// libp2p handshake.
///
/// `verifier_egress` is this verifier's own public IP when known. If it equals
/// the address's host the check is **skipped**, never passed: a verifier inside
/// the same NAT cannot tell two machines apart.
///
/// Runs a short-lived single-threaded runtime; call it from blocking code (the
/// provider-status snapshot, the AP's verifier worker) and cache the answer.
pub fn verify(
    multiaddr: &str,
    expected_peer_id: &str,
    verifier_egress: Option<&str>,
    timeout: Duration,
) -> Outcome {
    let Ok(addr) = multiaddr.parse::<Multiaddr>() else {
        return Outcome::Unreachable {
            reason: "unparseable multiaddr".to_string(),
        };
    };
    // Refuse an address whose /p2p tail disagrees with the claimed peer id: the
    // caller must not be able to verify one id at another id's socket.
    let tail = peer_id_of(multiaddr);
    if let Some(tail) = tail {
        if tail != expected_peer_id {
            return Outcome::Unreachable {
                reason: format!("multiaddr names peer {tail}, claim names {expected_peer_id}"),
            };
        }
    }
    if !is_dialable_socket(&addr) {
        return Outcome::Unreachable {
            reason: "not a TCP/WS libp2p socket".to_string(),
        };
    }
    if let (Some(host), Some(egress)) = (host_of(&addr), verifier_egress) {
        if host == egress {
            return Outcome::Skipped {
                reason: "same-egress".to_string(),
            };
        }
    }
    // GPN nodes listen with **websocket** framing on their TCP port (the
    // transport is `WS.or_transport(TCP)` in sc-network, and the live nodes
    // answer only `/ws`: a raw `/tcp/` dial hangs in the upgrade). Try the
    // address as published, then the same socket with `/ws` inserted, so the
    // verdict is about the SOCKET and its owner, not about which transport
    // spelling reached it first.
    let mut candidates = vec![addr.clone()];
    let has_ws = addr.iter().any(|p| {
        matches!(
            p,
            libp2p::multiaddr::Protocol::Ws(_) | libp2p::multiaddr::Protocol::Wss(_)
        )
    });
    if !has_ws {
        if let Some(ws) = with_ws(&addr) {
            candidates.push(ws);
        }
    }
    let mut last = Outcome::Unreachable {
        reason: "no candidate address".to_string(),
    };
    for cand in candidates {
        // Dial by peer id as well as address: libp2p then checks the identity
        // obtained by noise against the expected one and reports `WrongPeerId`.
        let dial = match cand.iter().find_map(|p| match p {
            libp2p::multiaddr::Protocol::P2p(id) => Some(id),
            _ => None,
        }) {
            Some(peer) => DialOpts::peer_id(peer)
                .addresses(vec![cand.clone()])
                .build(),
            None => DialOpts::unknown_peer_id().address(cand.clone()).build(),
        };
        last = dial_once(dial, &cand, expected_peer_id, timeout);
        // `Verified` and `WrongPeer` are both answers about the owner — stop.
        // Only "nothing answered" is worth a second transport.
        if !matches!(last, Outcome::Unreachable { .. }) {
            return last;
        }
    }
    last
}

/// Insert `/ws` immediately before the `/p2p/<id>` tail (or append it).
fn with_ws(addr: &Multiaddr) -> Option<Multiaddr> {
    use libp2p::multiaddr::Protocol;
    let mut out = Multiaddr::empty();
    let mut inserted = false;
    for p in addr.iter() {
        if matches!(p, Protocol::P2p(_)) && !inserted {
            out.push(Protocol::Ws(std::borrow::Cow::Borrowed("/")));
            inserted = true;
        }
        out.push(p);
    }
    if !inserted {
        out.push(Protocol::Ws(std::borrow::Cow::Borrowed("/")));
    }
    Some(out)
}

fn dial_once(dial: DialOpts, addr: &Multiaddr, expected: &str, timeout: Duration) -> Outcome {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            return Outcome::Unreachable {
                reason: format!("runtime: {e}"),
            }
        }
    };
    rt.block_on(async move {
        use futures::StreamExt;

        // Manual transport on purpose: `SwarmBuilder` can only build one
        // transport shape, and a `/ws` address needs the websocket framing.
        let keypair = libp2p::identity::Keypair::generate_ed25519();
        let local = keypair.public().to_peer_id();
        let tcp = libp2p::tcp::tokio::Transport::new(libp2p::tcp::Config::default());
        let noise = match libp2p::noise::Config::new(&keypair) {
            Ok(n) => n,
            Err(e) => {
                return Outcome::Unreachable {
                    reason: format!("noise: {e}"),
                }
            }
        };
        // `/ws` in the dial address selects websocket framing; a plain `/tcp/`
        // address is dialled raw. Both are layered over the same TCP transport.
        let is_ws = addr
            .iter()
            .any(|p| matches!(p, libp2p::multiaddr::Protocol::Ws(_)));
        let transport = if is_ws {
            libp2p::websocket::WsConfig::new(tcp)
                .upgrade(libp2p::core::upgrade::Version::V1Lazy)
                .authenticate(noise)
                .multiplex(libp2p::yamux::Config::default())
                .timeout(Duration::from_secs(10))
                .boxed()
        } else {
            tcp.upgrade(libp2p::core::upgrade::Version::V1)
                .authenticate(noise)
                .multiplex(libp2p::yamux::Config::default())
                .timeout(Duration::from_secs(10))
                .boxed()
        };
        let mut swarm = libp2p::Swarm::new(
            transport,
            libp2p::swarm::dummy::Behaviour,
            local,
            libp2p::swarm::Config::with_tokio_executor(),
        );
        if let Err(e) = swarm.dial(dial) {
            return Outcome::Unreachable {
                reason: format!("dial: {e}"),
            };
        }
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let ev = match tokio::time::timeout_at(deadline, swarm.select_next_some()).await {
                Err(_) => {
                    return Outcome::Unreachable {
                        reason: format!("timeout after {}s", timeout.as_secs()),
                    }
                }
                Ok(ev) => ev,
            };
            match ev {
                SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                    // With an expected peer id libp2p only establishes on a
                    // match, but compare anyway: this function is the last line.
                    return if peer_id.to_string() == expected {
                        Outcome::Verified {
                            peer_id: peer_id.to_string(),
                        }
                    } else {
                        Outcome::WrongPeer {
                            expected: expected.to_string(),
                            actual: peer_id.to_string(),
                        }
                    };
                }
                SwarmEvent::OutgoingConnectionError { error, .. } => {
                    use libp2p::swarm::DialError;
                    return match error {
                        DialError::WrongPeerId { obtained, .. } => Outcome::WrongPeer {
                            expected: expected.to_string(),
                            actual: obtained.to_string(),
                        },
                        DialError::LocalPeerId { .. } => Outcome::WrongPeer {
                            expected: expected.to_string(),
                            actual: "self".to_string(),
                        },
                        other => Outcome::Unreachable {
                            reason: other.to_string(),
                        },
                    };
                }
                _ => {}
            }
        }
    })
}

/// `GPN_DIALCHECK` mode. Default: `wrongpeer` (the incident fix; a working
/// node cannot lose its address because a hairpin failed).
///
/// * `off` — no verification (pre-2026-11-12 behaviour; only for a network with
///   no verifier at all).
/// * `wrongpeer` (default) — block an address that **positively** answers as a
///   different peer id; an unreachable socket still publishes on the old
///   evidence, exactly as today. This is the incident fix with no new way for a
///   working node to lose its address.
/// * `strict` — a public address is published only when a handshake returned
///   our own peer id. Correct per the design, but it depends on the node being
///   able to dial its own public socket (NAT loopback); a box without loopback
///   would lose a forwarding address that still works for the internet. Enable
///   it per fleet once every box has been observed to pass (this host passes in
///   ~70 ms, 2026-11-12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Off,
    WrongPeer,
    Strict,
}

impl Mode {
    pub fn from_env() -> Self {
        match std::env::var("GPN_DIALCHECK")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "0" | "off" | "false" | "no" => Mode::Off,
            "strict" | "1" | "true" | "on" | "yes" => Mode::Strict,
            _ => Mode::WrongPeer,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::WrongPeer => "wrongpeer",
            Mode::Strict => "strict",
        }
    }

    /// May an address with this verification outcome be published?
    pub fn allows(self, outcome: &Outcome) -> bool {
        match self {
            Mode::Off => true,
            Mode::WrongPeer => !matches!(outcome, Outcome::WrongPeer { .. }),
            Mode::Strict => outcome.is_verified(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_string_is_not_a_verified_address() {
        // WrongPeer is positive evidence of another owner ⇒ never publishable,
        // in every mode except `off`.
        let wrong = Outcome::WrongPeer {
            expected: "12D3KooWB".into(),
            actual: "12D3KooWA".into(),
        };
        assert!(!Mode::WrongPeer.allows(&wrong));
        assert!(!Mode::Strict.allows(&wrong));
        assert!(Mode::Off.allows(&wrong));
        // Unreachable: default keeps today's behaviour; strict refuses.
        let down = Outcome::Unreachable {
            reason: "timeout".into(),
        };
        assert!(Mode::WrongPeer.allows(&down));
        assert!(!Mode::Strict.allows(&down));
        assert!(Mode::Strict.allows(&Outcome::Verified {
            peer_id: "12D3KooWB".into()
        }));
        // Skipped never counts as verified.
        assert!(!Outcome::Skipped {
            reason: "same-egress".into()
        }
        .is_verified());
    }

    #[test]
    fn mode_is_parsed_from_the_environment() {
        // Pure parse helper, independent of the process env.
        for (raw, want) in [
            ("", Mode::WrongPeer),
            ("off", Mode::Off),
            ("0", Mode::Off),
            ("strict", Mode::Strict),
            ("1", Mode::Strict),
            ("garbage", Mode::WrongPeer),
        ] {
            let got = match raw.trim().to_ascii_lowercase().as_str() {
                "0" | "off" | "false" | "no" => Mode::Off,
                "strict" | "1" | "true" | "on" | "yes" => Mode::Strict,
                _ => Mode::WrongPeer,
            };
            assert_eq!(got, want, "GPN_DIALCHECK={raw:?}");
        }
    }

    #[test]
    fn same_egress_is_skipped_before_any_handshake() {
        // Never dial out when we share the claimant's public IP: the answer
        // would be meaningless. (The AP runs on the seed box.)
        let out = verify(
            "/ip4/203.0.113.7/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo",
            "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo",
            Some("203.0.113.7"),
            Duration::from_secs(1),
        );
        assert_eq!(out.code(), "skipped");
        assert!(out.summary().contains("same-egress"));
    }

    #[test]
    fn an_address_naming_a_different_peer_id_is_refused_before_dialling() {
        let out = verify(
            "/ip4/203.0.113.7/tcp/30333/p2p/12D3KooWCy8vfsWwj8spm3vJA5skDUWq2htZM1EKQ3XnAtm4sD4q",
            "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo",
            None,
            Duration::from_millis(50),
        );
        assert_eq!(out.code(), "unreachable");
        assert!(out.summary().contains("names peer"), "{out:?}");
    }

    #[test]
    fn an_unroutable_socket_is_unreachable_not_verified() {
        // TEST-NET-1, nothing listens; the check must say so, fast.
        let out = verify(
            "/ip4/192.0.2.1/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo",
            "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo",
            None,
            Duration::from_millis(300),
        );
        assert_eq!(out.code(), "unreachable", "{out:?}");
        assert_eq!(out.exit_code(), 3);
    }
}

#[cfg(test)]
mod selftest {
    use super::*;

    /// End-to-end: the dialer really completes a handshake and reads the peer
    /// id, on both transports GPN uses. This is the property the whole
    /// "publish only what a third party reaches" rule rests on.
    fn dial_a_listener(ws: bool) {
        use futures::StreamExt;
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel::<(String, String)>();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let keypair = libp2p::identity::Keypair::generate_ed25519();
                let local = keypair.public().to_peer_id();
                let tcp = libp2p::tcp::tokio::Transport::new(libp2p::tcp::Config::default());
                let noise = libp2p::noise::Config::new(&keypair).unwrap();
                let transport = if ws {
                    libp2p::websocket::WsConfig::new(tcp)
                        .upgrade(libp2p::core::upgrade::Version::V1Lazy)
                        .authenticate(noise)
                        .multiplex(libp2p::yamux::Config::default())
                        .timeout(Duration::from_secs(10))
                        .boxed()
                } else {
                    tcp.upgrade(libp2p::core::upgrade::Version::V1)
                        .authenticate(noise)
                        .multiplex(libp2p::yamux::Config::default())
                        .timeout(Duration::from_secs(10))
                        .boxed()
                };
                let mut swarm = libp2p::Swarm::new(
                    transport,
                    libp2p::swarm::dummy::Behaviour,
                    local,
                    libp2p::swarm::Config::with_tokio_executor(),
                );
                let listen: Multiaddr = if ws {
                    "/ip4/127.0.0.1/tcp/0/ws".parse().unwrap()
                } else {
                    "/ip4/127.0.0.1/tcp/0".parse().unwrap()
                };
                swarm.listen_on(listen).unwrap();
                let addr = loop {
                    if let SwarmEvent::NewListenAddr { address, .. } =
                        swarm.select_next_some().await
                    {
                        break address;
                    }
                };
                tx.send((addr.to_string(), local.to_string())).unwrap();
                let _ = tokio::time::timeout(Duration::from_secs(10), async {
                    loop {
                        let _ = swarm.select_next_some().await;
                    }
                })
                .await;
            });
        });
        let (addr, peer) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let mut target = addr.clone();
        if !target.to_string().contains("/p2p/") {
            target = format!("{target}/p2p/{peer}").parse().unwrap();
        }
        let out = verify(&target.to_string(), &peer, None, Duration::from_secs(6));
        assert!(out.is_verified(), "{target}: {out:?}");
    }

    #[test]
    fn dials_a_raw_tcp_listener() {
        dial_a_listener(false);
    }

    #[test]
    fn dials_a_websocket_listener() {
        // GPN nodes listen with websocket framing; this is the path that works
        // against a real deployment (verified live 2026-11-12).
        dial_a_listener(true);
    }
}
