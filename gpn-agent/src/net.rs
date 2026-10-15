//! Small network helpers shared by the GPN agent binaries.
//!
//! ## Why this exists: the "host-DNS /result defect"
//!
//! Some GPN best-effort HTTP callbacks (the provider → AP `/result` return, and
//! the AP → provider `/assign` push) target a **public** name such as
//! `gpn-testnet.duckdns.org`. On a host whose resolver does not answer **AAAA**
//! queries (e.g. a laggy `192.168.1.1` router resolver), a dual-stack client
//! (`getaddrinfo(AF_UNSPEC)`) blocks on the AAAA leg for ~15s before falling
//! back to the A record:
//!
//! ```text
//! $ getent ahostsv6 gpn-testnet.duckdns.org   # rc=124 (timeout)
//! $ curl    http://gpn-testnet.duckdns.org:8092/health  # http=000 (8s stall)
//! $ curl -4 http://gpn-testnet.duckdns.org:8092/health  # http=200 (0.02s)
//! ```
//!
//! Because the `/result` callback is best-effort with a hard 5s budget, that
//! stall makes it time out and the AP falls back to its ~11s on-chain reconcile,
//! blowing the <5s e2e target. `reqwest::blocking::Client` uses AF_UNSPEC by
//! default, so it inherits this stall.
//!
//! The cure is to resolve **A (IPv4) records only** — never emit an AAAA query
//! — and to bound the connect attempt. [`ipv4_only_client`] returns a
//! `reqwest::blocking::Client` that does exactly that, so a slow/absent AAAA
//! can never block the request. The real host fix
//! (`sudo resolvectl dns <iface> 1.1.1.1 && resolvectl flush-caches`) still
//! helps every other client, but this keeps GPN robust without root.

#[cfg(unix)]
use std::ffi::CString;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

/// A `reqwest` DNS resolver that performs **AF_INET (A) lookups only**.
///
/// It deliberately avoids `getaddrinfo(AF_UNSPEC)`, which blocks on hosts whose
/// resolver does not answer AAAA. `getaddrinfo(AF_INET, …)` returns immediately
/// (verified: 0.0s vs 15s for AF_UNSPEC on the affected host).
///
/// The port is left as 0 — `reqwest` overrides it with the explicit port in the
/// request URL (e.g. `:8092`), per the `Resolve` trait contract.
#[derive(Debug, Default, Clone, Copy)]
pub struct Ipv4OnlyResolver;

impl reqwest::dns::Resolve for Ipv4OnlyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs = ipv4_addrs(&host)?;
            let iter: reqwest::dns::Addrs = Box::new(addrs.into_iter());
            Ok(iter)
        })
    }
}

/// Resolve `host` to its IPv4 (`A`) addresses using an `AF_INET`-only
/// `getaddrinfo`, so no AAAA query is ever issued. Returns an empty iterator
/// (not an error) when the name has no A record, letting the caller surface a
/// normal connect error instead of a resolution error.
fn ipv4_addrs(host: &str) -> Result<Vec<SocketAddr>, Box<dyn std::error::Error + Send + Sync>> {
    // Fast path: an IPv4 literal needs no lookup at all.
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return Ok(vec![SocketAddr::V4(SocketAddrV4::new(ip, 0))]);
    }
    resolve_ipv4_only(host)
}

/// Unix (`libc`) AF_INET-only resolution — never issues an AAAA query.
#[cfg(unix)]
fn resolve_ipv4_only(
    host: &str,
) -> Result<Vec<SocketAddr>, Box<dyn std::error::Error + Send + Sync>> {
    let c_host = CString::new(host)?;
    let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
    hints.ai_family = libc::AF_INET; // A-only — never queries AAAA
    hints.ai_socktype = libc::SOCK_STREAM;
    hints.ai_protocol = libc::IPPROTO_TCP;

    let mut res: *mut libc::addrinfo = std::ptr::null_mut();
    let rc = unsafe { libc::getaddrinfo(c_host.as_ptr(), std::ptr::null(), &hints, &mut res) };
    if rc != 0 {
        let msg = unsafe {
            std::ffi::CStr::from_ptr(libc::gai_strerror(rc))
                .to_string_lossy()
                .into_owned()
        };
        return Err(format!("IPv4 getaddrinfo for '{host}' failed: {msg}").into());
    }

    let mut out = Vec::new();
    let mut cur = res;
    while !cur.is_null() {
        let ai = unsafe { &*cur };
        if ai.ai_family == libc::AF_INET && !ai.ai_addr.is_null() {
            let sin = unsafe { &*(ai.ai_addr as *const libc::sockaddr_in) };
            let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
            out.push(SocketAddr::V4(SocketAddrV4::new(ip, 0)));
        }
        cur = ai.ai_next;
    }
    unsafe { libc::freeaddrinfo(res) };
    Ok(out)
}

/// Windows AF_INET-only resolution via `GetAddrInfoW` — the `libc` crate does
/// not export the Winsock `getaddrinfo`/`addrinfo`/`sockaddr_in` symbols on
/// Windows, so we bind the Win32 API directly through `windows-sys`. Same
/// semantics: `AF_INET` hints, so no AAAA query is ever sent.
#[cfg(windows)]
fn resolve_ipv4_only(
    host: &str,
) -> Result<Vec<SocketAddr>, Box<dyn std::error::Error + Send + Sync>> {
    use windows_sys::Win32::Networking::WinSock::{
        FreeAddrInfoW, GetAddrInfoW, ADDRINFOW, AF_INET, IPPROTO_TCP, SOCKADDR_IN, SOCK_STREAM,
    };

    // `GetAddrInfoW` takes a NUL-terminated UTF-16 node name.
    let mut node: Vec<u16> = host.encode_utf16().collect();
    node.push(0);

    let mut hints: ADDRINFOW = unsafe { std::mem::zeroed() };
    hints.ai_family = AF_INET as i32; // A-only — never queries AAAA
    hints.ai_socktype = SOCK_STREAM;
    hints.ai_protocol = IPPROTO_TCP;

    let mut res: *mut ADDRINFOW = std::ptr::null_mut();
    let rc = unsafe { GetAddrInfoW(node.as_ptr(), std::ptr::null(), &hints, &mut res) };
    if rc != 0 {
        // `windows-sys` does not export `gai_strerror`, so surface the code.
        return Err(format!("IPv4 GetAddrInfoW for '{host}' failed: {rc}").into());
    }

    let mut out = Vec::new();
    let mut cur = res;
    while !cur.is_null() {
        let ai = unsafe { &*cur };
        if ai.ai_family == AF_INET as i32 && !ai.ai_addr.is_null() {
            let sin = unsafe { &*(ai.ai_addr as *const SOCKADDR_IN) };
            let ip = Ipv4Addr::from(u32::from_be(unsafe { sin.sin_addr.S_un.S_addr }));
            out.push(SocketAddr::V4(SocketAddrV4::new(ip, 0)));
        }
        cur = ai.ai_next;
    }
    unsafe { FreeAddrInfoW(res) };
    Ok(out)
}

/// Portable fallback for any other target: `ToSocketAddrs` filtered to IPv4.
#[cfg(not(any(unix, windows)))]
fn resolve_ipv4_only(
    host: &str,
) -> Result<Vec<SocketAddr>, Box<dyn std::error::Error + Send + Sync>> {
    use std::net::ToSocketAddrs;
    let addrs = (host, 0u16)
        .to_socket_addrs()?
        .filter(SocketAddr::is_ipv4)
        .map(|mut a| {
            a.set_port(0);
            a
        })
        .collect();
    Ok(addrs)
}

/// Build a blocking HTTP client that (a) resolves **IPv4 only** (no AAAA stall),
/// (b) bounds the TCP connect, and (c) bounds the whole request. Used for the
/// GPN best-effort callbacks that may target a public `*.duckdns.org` name.
pub fn ipv4_only_client(timeout: Duration) -> reqwest::Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .dns_resolver(std::sync::Arc::new(Ipv4OnlyResolver))
        .connect_timeout(timeout.min(Duration::from_secs(3)))
        .timeout(timeout)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_literal_short_circuits() {
        let addrs = ipv4_addrs("127.0.0.1").unwrap();
        assert_eq!(addrs, vec![SocketAddr::from(([127, 0, 0, 1], 0))]);
    }

    #[test]
    fn ipv4_only_addrs_are_all_v4() {
        // A well-known A record; must never yield a v6 address. (Network-gated:
        // skip silently if DNS is unavailable in the sandbox.)
        if let Ok(addrs) = ipv4_addrs("one.one.one.one") {
            assert!(addrs.iter().all(SocketAddr::is_ipv4));
        }
    }

    #[test]
    fn builder_constructs_with_ipv4_resolver() {
        let c = ipv4_only_client(Duration::from_secs(5));
        assert!(c.is_ok(), "ipv4_only_client failed to build: {:?}", c.err());
    }
}
