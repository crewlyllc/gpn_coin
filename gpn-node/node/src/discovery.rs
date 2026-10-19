//! Bitcoin-style peer discovery for the GPN node.
//!
//! GPN historically shipped with a single, static `--bootnodes <multiaddr>`
//! hardcoded in the operator's launch command. That is a single point of
//! failure: if that one node is unreachable, a fresh node never joins the
//! network. This module builds discovery *on top* of the static bootnode:
//!
//! 1. **Persistent peer store (addrman)** — known-good peers are persisted to
//!    `<base-path>/gpn-peers.json` and re-dialed as bootnodes on the next
//!    start. (Substrate's `sc-network` keeps no on-disk address book in this
//!    version, so this is our minimal equivalent.)
//! 2. **DNS seed** — `--bootnodes-dns <name>` (Bitcoin-style): resolve a DNS
//!    name and read the peer multiaddrs it publishes as TXT records.
//! 3. **HTTP(S) seed list** — `--bootnodes-url <url>`: fetch a published
//!    peer-list JSON. The default source is the live network directory at
//!    <https://gpncoin.ai/seeds.json> (see [`DEFAULT_SEED_URL`]).
//! 4. **On-chain discovery** — read the `gpn-liveness` full-node registry and
//!    `ComputeMarketplace::ProviderEndpoints` and dial them (see `service.rs`).
//!
//! Every source is independent and best-effort: a source that is unreachable,
//! malformed or slow is logged and skipped — it **never** aborts startup.
//! The compiled-in bootnode (chain-spec / registry) remains the hard fallback.
//!
//! ## Seed-list wire format
//!
//! A seed source yields a list of libp2p multiaddresses, each carrying the
//! peer id (`…/p2p/12D3Koo…`). JSON payloads may be either:
//!
//! ```json
//! { "multiaddrs": ["/dns4/node1.example.org/tcp/30333/p2p/12D3Koo…", "…"] }
//! ```
//!
//! or a plain array:
//!
//! ```json
//! ["/dns4/node1.example.org/tcp/30333/p2p/12D3Koo…", "…"]
//! ```
//!
//! DNS TXT records and non-JSON bodies are also accepted as newline-separated
//! multiaddr strings (one per line; `#`-prefixed lines are comments) so a seed
//! can be as simple as a text file behind nginx.

use crate::network_registry::BootstrapSeed;
use sc_network::config::MultiaddrWithPeerId;
use sc_network::Multiaddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Filename (under the node's base path) of the persisted peer store.
pub const PEERS_FILE: &str = "gpn-peers.json";

/// Default `--bootnodes-url`: the live network directory served by the
/// gpncoin.ai Worker. It merges the compiled-in seed list with the access
/// point's currently-healthy providers. Failure is non-fatal — the compiled-in
/// bootnodes remain the hard fallback.
pub const DEFAULT_SEED_URL: &str = "https://gpncoin.ai/seeds.json";

/// Upper bound on how many peers we persist / carry over (keeps the file small
/// and the bootnode set dialable in a bounded time).
pub const MAX_PERSISTED_PEERS: usize = 128;

/// The CLI-side seed sources (mirrors the `--bootnodes-dns` / `--bootnodes-url`
/// flags). Kept free of `clap` so it stays unit-testable.
#[derive(Debug, Clone, Default)]
pub struct SeedSources {
    /// DNS names to resolve as TXT seeds.
    pub dns: Vec<String>,
    /// HTTP(S) seed-list URLs.
    pub urls: Vec<String>,
}

// ---------------------------------------------------------------------------
// Pure parsing / normalisation
// ---------------------------------------------------------------------------

/// Parse a seed payload into a list of candidate multiaddr strings.
///
/// Accepted inputs, in order of preference:
/// * JSON object `{ "multiaddrs": [ … ] }`
/// * JSON array `[ … ]`
/// * a bare JSON string holding newline-separated multiaddrs
/// * plain text: newline-separated multiaddrs (`#` comments ignored)
pub fn parse_seed_payload(text: &str) -> Vec<String> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
        match value {
            serde_json::Value::Array(items) => {
                let out: Vec<String> = items.into_iter().filter_map(json_string).collect();
                if !out.is_empty() {
                    return out;
                }
            }
            serde_json::Value::Object(map) => {
                if let Some(serde_json::Value::Array(items)) = map.get("multiaddrs") {
                    return items.iter().filter_map(json_string_ref).collect();
                }
            }
            serde_json::Value::String(s) => return split_lines(&s),
            _ => {}
        }
    }
    split_lines(text)
}

fn json_string(v: serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s),
        _ => None,
    }
}

fn json_string_ref(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// Split a free-form text body into candidate multiaddr lines. Only lines that
/// look like a multiaddr (start with `/`) are kept; `#` comments and blank
/// lines are ignored. Invalid entries are dropped later by the canonicaliser.
fn split_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| l.starts_with('/') && !l.starts_with("//"))
        .map(str::to_string)
        .collect()
}

/// Normalise a candidate into the canonical, *dialable* bootnode form
/// (`…/p2p/<peerid>`). Returns `None` when the entry is not a parseable
/// multiaddress or carries no peer id — such entries cannot be dialed as
/// libp2p peers and are dropped rather than causing an error.
pub fn canonical_bootnode(candidate: &str) -> Option<String> {
    let mw: MultiaddrWithPeerId = candidate.trim().parse().ok()?;
    Some(mw.to_string())
}

/// Convert a published endpoint into a multiaddress string, best-effort.
///
/// Handles, in order:
/// * an existing libp2p multiaddr (`/ip4/…`, `/dns4/…`, …) — returned as-is;
/// * an `http(s)://` / `ws(s)://` URL — host + port become `/dns4|ip4|ip6`;
/// * a bare `host:port` (IPv4, hostname or bracketed IPv6).
///
/// The result is *not* necessarily dialable: a URL-derived multiaddr has no
/// peer id. Callers must still run it through [`canonical_bootnode`] (or check
/// for a `/p2p/` suffix) before dialing.
pub fn endpoint_to_multiaddr(endpoint: &str) -> Option<String> {
    let e = endpoint.trim();
    if e.is_empty() {
        return None;
    }
    // Already a multiaddr?
    if e.starts_with('/') {
        let ma: Multiaddr = e.parse().ok()?;
        return Some(ma.to_string());
    }
    // URL or bare host:port. Strip scheme and any path/query.
    let without_scheme = match e.split_once("://") {
        Some((_scheme, rest)) => rest,
        None => e,
    };
    let authority = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme);
    let (host, port) = split_host_port(authority)?;
    Some(host_port_to_multiaddr(&host, port))
}

/// Split a URL authority (`user:pass@host:port`, `[v6]:port`, `host:port`) into
/// host + port.
fn split_host_port(authority: &str) -> Option<(String, u16)> {
    // Drop any userinfo.
    let hostport = authority.rsplit('@').next().unwrap_or(authority);
    if let Some(rest) = hostport.strip_prefix('[') {
        // Bracketed IPv6: [::1]:30333
        let end = rest.find(']')?;
        let host = rest[..end].to_string();
        let port = rest[end + 1..].strip_prefix(':')?.parse().ok()?;
        return Some((host, port));
    }
    let (host, port) = hostport.rsplit_once(':')?;
    if host.is_empty() {
        return None;
    }
    Some((host.to_string(), port.parse().ok()?))
}

/// Build a multiaddr for a hostname/literal-IP + port.
fn host_port_to_multiaddr(host: &str, port: u16) -> String {
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(_)) => format!("/ip4/{host}/tcp/{port}"),
        Ok(std::net::IpAddr::V6(_)) => format!("/ip6/{host}/tcp/{port}"),
        Err(_) => format!("/dns4/{host}/tcp/{port}"),
    }
}

/// Expand one hardcoded bootstrap seed ("Source A") into dialable multiaddrs.
///
/// `ip4` is authoritative; a `dns4` alias in the same entry is emitted **as
/// well**, so an IP change survives a DNS update without a client release
/// while the literal IP remains the cold-start fallback. An entry without a
/// valid `peer_id` yields nothing (logged) — a seed is a dial target, and the
/// peer id is what makes a dial to a wrong or hostile host useless.
pub fn bootstrap_seed_multiaddrs(seed: &BootstrapSeed) -> Vec<String> {
    let id = if seed.id.is_empty() {
        seed.peer_id.as_str()
    } else {
        seed.id.as_str()
    };
    let peer = seed.peer_id.trim();
    if peer.is_empty() {
        log::warn!("peer-discovery: bootstrap seed '{id}' has no peer_id — skipped");
        return Vec::new();
    }
    let port = if seed.port == 0 { 30333 } else { seed.port };

    let mut candidates: Vec<String> = Vec::new();
    let ip4 = seed.ip4.trim();
    if !ip4.is_empty() {
        match ip4.parse::<std::net::Ipv4Addr>() {
            Ok(parsed) => candidates.push(format!("/ip4/{parsed}/tcp/{port}/p2p/{peer}")),
            Err(_) => log::warn!(
                "peer-discovery: bootstrap seed '{id}' has a non-IPv4 'ip4' value '{ip4}' — \
                 that arm skipped"
            ),
        }
    }
    let dns4 = seed.dns4.trim();
    if !dns4.is_empty() {
        candidates.push(format!("/dns4/{dns4}/tcp/{port}/p2p/{peer}"));
    }
    if candidates.is_empty() {
        log::warn!("peer-discovery: bootstrap seed '{id}' has neither ip4 nor dns4 — skipped");
        return Vec::new();
    }

    // The parse *is* the validation (peer id + multiaddr syntax).
    let out: Vec<String> = candidates
        .into_iter()
        .filter_map(|c| canonical_bootnode(&c))
        .collect();
    if out.is_empty() {
        log::warn!("peer-discovery: bootstrap seed '{id}' is not dialable — skipped");
    }
    out
}

/// Expand every hardcoded seed into a flat, de-duplicated multiaddr list.
pub fn bootstrap_seed_addrs(seeds: &[BootstrapSeed]) -> Vec<String> {
    let groups: Vec<Vec<String>> = seeds.iter().map(bootstrap_seed_multiaddrs).collect();
    merge_dedupe(&groups)
}

/// Flatten several sources into one deduplicated, canonical bootnode list,
/// preserving first-seen order. Entries that are not dialable are dropped.
pub fn merge_dedupe(groups: &[Vec<String>]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for group in groups {
        for item in group {
            if let Some(canonical) = canonical_bootnode(item) {
                if seen.insert(canonical.clone()) {
                    out.push(canonical);
                }
            }
        }
    }
    out
}

/// Combine the already-configured bootnodes with the *results* of the extra
/// seed sources. A failed source (`Err`) is logged and ignored — this is the
/// guarantee that "a failing source must NOT abort startup".
pub fn merge_sources_lossy(
    existing: &[String],
    results: Vec<Result<Vec<String>, String>>,
) -> Vec<String> {
    let mut groups: Vec<Vec<String>> = vec![existing.to_vec()];
    for (idx, result) in results.into_iter().enumerate() {
        match result {
            Ok(v) => groups.push(v),
            Err(e) => log::warn!("peer-discovery: source #{idx} failed (ignored): {e}"),
        }
    }
    merge_dedupe(&groups)
}

// ---------------------------------------------------------------------------
// Persistent peer store (addrman)
// ---------------------------------------------------------------------------

/// Read the persisted peer store from `<base>/gpn-peers.json`. Missing or
/// corrupt files yield an empty list (never an error).
pub fn read_persisted_peers(base: &Path) -> Vec<String> {
    let path = base.join(PEERS_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => parse_seed_payload(&text),
        Err(_) => Vec::new(),
    }
}

/// Write the persisted peer store, deduplicated and bounded. Best-effort.
pub fn write_persisted_peers(base: &Path, addrs: &[String]) -> Result<(), String> {
    let list = merge_dedupe(&[addrs.to_vec()]);
    let list: Vec<String> = list.into_iter().take(MAX_PERSISTED_PEERS).collect();
    let doc = serde_json::json!({ "multiaddrs": list });
    let path = base.join(PEERS_FILE);
    let bytes = serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?;
    std::fs::write(&path, bytes).map_err(|e| format!("write {}: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// Runtime resolvers (best-effort; each returns Result, never panics)
// ---------------------------------------------------------------------------

/// Resolve a DNS seed name into multiaddr strings.
///
/// Each TXT record is treated as a seed payload (a JSON seed list, or plain
/// multiaddr text). Records are concatenated before parsing so a multiaddr that
/// straddles the 255-byte TXT chunk boundary still parses.
pub async fn resolve_dns_seed(name: &str) -> Result<Vec<String>, String> {
    use hickory_resolver::TokioResolver;

    let resolver = TokioResolver::builder_tokio()
        .map_err(|e| format!("resolver init: {e}"))?
        .build();
    let lookup = resolver
        .txt_lookup(name)
        .await
        .map_err(|e| format!("TXT lookup {name}: {e}"))?;

    let mut out = Vec::new();
    // `TxtLookup` yields `&TXT` records directly.
    for txt in lookup.iter() {
        let joined: String = txt
            .txt_data()
            .iter()
            .map(|chunk| String::from_utf8_lossy(chunk))
            .collect();
        out.extend(parse_seed_payload(&joined));
    }
    Ok(out)
}

/// Fetch and parse a published seed-list JSON over HTTP or HTTPS.
///
/// `http://` uses attohttpc (no TLS stack). `https://` uses a minimal
/// HTTP/1.1 GET over rustls — the default directory
/// ([`DEFAULT_SEED_URL`]) is HTTPS, and the node must not depend on the host
/// having a TLS client or a proxy. Both paths are bounded by a 20 s timeout and
/// return `Err` on any failure; callers log and continue.
pub async fn fetch_seed_url(url: &str) -> Result<Vec<String>, String> {
    let trimmed = url.trim();
    let text = if trimmed.to_ascii_lowercase().starts_with("https://") {
        tokio::time::timeout(Duration::from_secs(20), https_get_text(trimmed))
            .await
            .map_err(|_| format!("GET {trimmed}: timed out"))??
    } else {
        let url = trimmed.to_string();
        tokio::task::spawn_blocking(move || {
            let response = attohttpc::get(&url)
                .timeout(Duration::from_secs(20))
                .send()
                .map_err(|e| format!("GET {url}: {e}"))?;
            if !response.status().is_success() {
                return Err(format!("GET {url}: HTTP {}", response.status()));
            }
            response.text().map_err(|e| format!("read body {url}: {e}"))
        })
        .await
        .map_err(|e| format!("join error: {e}"))??
    };
    Ok(parse_seed_payload(&text))
}

/// Minimal HTTPS GET (HTTP/1.1 over rustls, `Connection: close`).
///
/// Deliberately tiny: the seed client needs one request per source at startup,
/// so a full HTTP client is unnecessary. Redirects are treated as an error
/// (publish the final URL); compressed bodies are avoided by asking for
/// `identity`.
async fn https_get_text(url: &str) -> Result<String, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let parsed = url::Url::parse(url).map_err(|e| format!("parse {url}: {e}"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| format!("{url}: no host"))?
        .to_string();
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| format!("{url}: no port"))?;
    let mut target = parsed.path().to_string();
    if target.is_empty() {
        target.push('/');
    }
    if let Some(query) = parsed.query() {
        target.push('?');
        target.push_str(query);
    }

    let tcp = tokio::net::TcpStream::connect((host.as_str(), port))
        .await
        .map_err(|e| format!("connect {host}:{port}: {e}"))?;
    let server_name = rustls::pki_types::ServerName::try_from(host.clone())
        .map_err(|e| format!("bad server name {host}: {e}"))?;
    let connector = tokio_rustls::TlsConnector::from(tls_client_config()?);
    let mut stream = connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| format!("TLS handshake {host}: {e}"))?;

    let request = format!(
        "GET {target} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: gpn-node/{}\r\n\
         Accept: application/json, text/plain, */*\r\nAccept-Encoding: identity\r\n\
         Connection: close\r\n\r\n",
        env!("CARGO_PKG_VERSION")
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| format!("write {url}: {e}"))?;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .map_err(|e| format!("read {url}: {e}"))?;
    parse_http_response(&raw)
}

/// Shared rustls client config: Mozilla's webpki root store, ring provider.
fn tls_client_config() -> Result<Arc<rustls::ClientConfig>, String> {
    static CONFIG: std::sync::OnceLock<Arc<rustls::ClientConfig>> = std::sync::OnceLock::new();
    if let Some(config) = CONFIG.get() {
        return Ok(config.clone());
    }
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("tls config: {e}"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let config = Arc::new(config);
    let _ = CONFIG.set(config.clone());
    Ok(config)
}

/// Parse one HTTP/1.1 response: status line, headers, body (incl. chunked).
fn parse_http_response(raw: &[u8]) -> Result<String, String> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "malformed HTTP response: no header terminator".to_string())?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let body = &raw[split + 4..];

    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| format!("bad status line: {status_line:?}"))?;
    if !(200..300).contains(&code) {
        return Err(format!("HTTP {code}"));
    }

    let mut chunked = false;
    let mut content_length: Option<usize> = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            match name.trim().to_ascii_lowercase().as_str() {
                "transfer-encoding" => {
                    chunked = value.trim().to_ascii_lowercase().contains("chunked")
                }
                "content-length" => content_length = value.trim().parse().ok(),
                _ => {}
            }
        }
    }

    let body = if chunked {
        decode_chunked(body)?
    } else {
        match content_length {
            Some(n) if n <= body.len() => body[..n].to_vec(),
            _ => body.to_vec(),
        }
    };
    String::from_utf8(body).map_err(|e| format!("non-UTF-8 body: {e}"))
}

/// Decode `Transfer-Encoding: chunked`.
fn decode_chunked(mut data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let line_end = data
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| "chunked body: missing chunk size".to_string())?;
        let size_line = String::from_utf8_lossy(&data[..line_end]);
        let size_hex = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16)
            .map_err(|_| format!("chunked body: bad chunk size {size_hex:?}"))?;
        data = &data[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if data.len() < size {
            return Err("chunked body: truncated chunk".to_string());
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size..];
        if data.starts_with(b"\r\n") {
            data = &data[2..];
        }
    }
}

// ---------------------------------------------------------------------------
// On-chain storage helpers (pure; used by `service.rs` to read the registry)
// ---------------------------------------------------------------------------

/// The 32-byte FRAME storage prefix for `twox128(pallet) ++ twox128(item)`.
pub fn storage_prefix(pallet: &[u8], item: &[u8]) -> Vec<u8> {
    let mut prefix = Vec::with_capacity(32);
    prefix.extend_from_slice(&sp_io::hashing::twox_128(pallet));
    prefix.extend_from_slice(&sp_io::hashing::twox_128(item));
    prefix
}

/// Decode a SCALE-encoded byte string value. Works for both `Vec<u8>` and
/// `BoundedVec<u8, N>` (identical encoding).
pub fn decode_bytes_value(data: &[u8]) -> Option<Vec<u8>> {
    use codec::Decode;
    Vec::<u8>::decode(&mut &data[..]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "/dns4/node1.example.org/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo";
    const B: &str =
        "/ip4/203.0.113.7/tcp/30334/p2p/12D3KooW9ye8kXwMYT4vBJoJdCkM9VfRrcM7nGgqVDPq3Fy8i6xw";

    #[test]
    fn parse_object_form() {
        let payload = format!(r#"{{"multiaddrs": ["{A}", "{B}"]}}"#);
        assert_eq!(
            parse_seed_payload(&payload),
            vec![A.to_string(), B.to_string()]
        );
    }

    #[test]
    fn parse_array_form() {
        let payload = format!(r#"["{A}", "{B}"]"#);
        assert_eq!(
            parse_seed_payload(&payload),
            vec![A.to_string(), B.to_string()]
        );
    }

    #[test]
    fn parse_plaintext_lines_and_comments() {
        let payload = format!("# seed list\n{A}\n\n  {B}  \n");
        assert_eq!(
            parse_seed_payload(&payload),
            vec![A.to_string(), B.to_string()]
        );
    }

    #[test]
    fn parse_json_string_of_lines() {
        let payload = format!("\"{A}\\n{B}\"");
        assert_eq!(
            parse_seed_payload(&payload),
            vec![A.to_string(), B.to_string()]
        );
    }

    #[test]
    fn parse_garbage_yields_nothing() {
        assert!(parse_seed_payload("not a seed at all").is_empty());
        assert!(parse_seed_payload("").is_empty());
    }

    #[test]
    fn canonical_accepts_peerid_multiaddr() {
        assert_eq!(canonical_bootnode(A).as_deref(), Some(A));
        assert_eq!(canonical_bootnode(&format!("  {A} ")).as_deref(), Some(A));
    }

    #[test]
    fn canonical_rejects_missing_peerid() {
        assert_eq!(canonical_bootnode("/ip4/1.2.3.4/tcp/30333"), None);
        assert_eq!(canonical_bootnode("http://example.org:30333"), None);
        assert_eq!(canonical_bootnode(""), None);
        assert_eq!(canonical_bootnode("total garbage"), None);
    }

    #[test]
    fn endpoint_multiaddr_passthrough() {
        assert_eq!(endpoint_to_multiaddr(A).as_deref(), Some(A));
        assert_eq!(
            endpoint_to_multiaddr("/ip4/127.0.0.1/tcp/30333").as_deref(),
            Some("/ip4/127.0.0.1/tcp/30333")
        );
    }

    #[test]
    fn endpoint_from_http_url() {
        assert_eq!(
            endpoint_to_multiaddr("http://gpn-testnet.duckdns.org:8093/status").as_deref(),
            Some("/dns4/gpn-testnet.duckdns.org/tcp/8093")
        );
        assert_eq!(
            endpoint_to_multiaddr("ws://203.0.113.9:9944/path?x=1").as_deref(),
            Some("/ip4/203.0.113.9/tcp/9944")
        );
        assert_eq!(
            endpoint_to_multiaddr("https://[2001:db8::1]:30333").as_deref(),
            Some("/ip6/2001:db8::1/tcp/30333")
        );
    }

    #[test]
    fn endpoint_from_host_port() {
        assert_eq!(
            endpoint_to_multiaddr("example.org:30333").as_deref(),
            Some("/dns4/example.org/tcp/30333")
        );
        assert_eq!(
            endpoint_to_multiaddr("198.51.100.4:30333").as_deref(),
            Some("/ip4/198.51.100.4/tcp/30333")
        );
        assert_eq!(endpoint_to_multiaddr(""), None);
        assert_eq!(endpoint_to_multiaddr("no-port"), None);
    }

    #[test]
    fn merge_preserves_order_and_dedupes() {
        let groups = vec![
            vec![A.to_string(), B.to_string()],
            vec![A.to_string(), "garbage".to_string()],
            vec![],
        ];
        assert_eq!(merge_dedupe(&groups), vec![A.to_string(), B.to_string()]);
    }

    #[test]
    fn merge_keeps_valid_from_later_group() {
        let c =
            "/ip4/198.51.100.9/tcp/30333/p2p/12D3KooWB9tGm4vCnQ7uF7bH6kK3sXyNcZ9rQ8tW5eL2pY4dR1aZ";
        let groups = vec![vec![A.to_string()], vec![c.to_string()]];
        let merged = merge_dedupe(&groups);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[1], c);
    }

    #[test]
    fn failing_source_does_not_abort_or_drop_existing() {
        let result = merge_sources_lossy(
            &[A.to_string()],
            vec![
                Err("DNS NXDOMAIN".to_string()),
                Ok(vec![B.to_string()]),
                Err("connection refused".to_string()),
            ],
        );
        assert_eq!(result, vec![A.to_string(), B.to_string()]);
    }

    #[test]
    fn storage_prefix_is_two_twox128_hashes() {
        let p = storage_prefix(b"Liveness", b"Endpoints");
        assert_eq!(p.len(), 32);
        assert_eq!(&p[..16], &sp_io::hashing::twox_128(b"Liveness")[..]);
        assert_eq!(&p[16..], &sp_io::hashing::twox_128(b"Endpoints")[..]);
    }

    #[test]
    fn decode_bytes_value_handles_scale_vec() {
        // SCALE: compact length 3 (0x0c) + bytes.
        assert_eq!(
            decode_bytes_value(&[0x0c, b'a', b'b', b'c']),
            Some(b"abc".to_vec())
        );
        // Empty vec: compact 0 (0x00).
        assert_eq!(decode_bytes_value(&[0x00]), Some(vec![]));
        assert_eq!(decode_bytes_value(&[]), None);
    }

    #[test]
    fn persisted_store_roundtrip() {
        let dir = std::env::temp_dir().join(format!("gpn-disc-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        write_persisted_peers(&dir, &[A.to_string(), A.to_string(), B.to_string()]).unwrap();
        let back = read_persisted_peers(&dir);
        assert_eq!(back, vec![A.to_string(), B.to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_persisted_store_is_empty() {
        let dir = std::env::temp_dir().join("gpn-disc-nonexistent-dir-xyz");
        assert!(read_persisted_peers(&dir).is_empty());
    }

    #[tokio::test]
    async fn fetch_seed_url_errors_do_not_panic() {
        // Port 1 on loopback is refused immediately — no external network needed.
        let r = fetch_seed_url("http://127.0.0.1:1/seed.json").await;
        assert!(r.is_err(), "expected a connection error, got {r:?}");
    }

    #[test]
    fn http_response_content_length() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\n{\"a\":1}";
        assert_eq!(parse_http_response(raw).unwrap(), "{\"a\":1}");
    }

    #[test]
    fn http_response_chunked() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n";
        assert_eq!(parse_http_response(raw).unwrap(), "{\"a\":1}");
    }

    #[test]
    fn http_response_non_200_is_error() {
        let raw = b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n";
        assert!(parse_http_response(raw).is_err());
        assert!(parse_http_response(b"garbage").is_err());
    }

    #[test]
    fn directory_payload_parses_as_seed_list() {
        // The exact shape served by https://gpncoin.ai/seeds.json.
        let payload = format!(
            r#"{{"network":"GPN Public Testnet","genesis":"0xab","chain_id":"gpn_testnet","updated_at":"2026-10-19T22:00:00Z","ttl":60,"multiaddrs":["{A}"],"nodes":[{{"multiaddr":"{A}","role":"seed"}}]}}"#
        );
        assert_eq!(parse_seed_payload(&payload), vec![A.to_string()]);
    }

    #[test]
    fn default_seed_url_is_the_public_directory() {
        assert_eq!(DEFAULT_SEED_URL, "https://gpncoin.ai/seeds.json");
        assert!(DEFAULT_SEED_URL.starts_with("https://"));
    }

    // Network smoke tests. Ignored by default so `cargo test` stays offline:
    //   cargo test -p gpn-node --offline discovery::tests::live -- --ignored
    #[tokio::test]
    #[ignore = "network: HTTPS GET + TLS stack smoke test"]
    async fn live_https_get_smoke() {
        let body = https_get_text("https://example.com/")
            .await
            .expect("https GET example.com");
        assert!(
            body.contains("Example Domain"),
            "unexpected body: {body:.200}"
        );
    }

    #[tokio::test]
    #[ignore = "network: fetches the deployed network directory"]
    async fn live_directory_fetch_smoke() {
        let seeds = fetch_seed_url(DEFAULT_SEED_URL)
            .await
            .expect("fetch the live directory");
        assert!(
            !seeds.is_empty(),
            "directory returned no dialable multiaddrs"
        );
        assert!(seeds.iter().all(|s| s.contains("/p2p/")));
    }
}

#[cfg(test)]
mod bootstrap_seed_tests {
    use super::*;

    fn seed(id: &str, ip4: &str, port: u16, peer: &str, dns4: &str) -> BootstrapSeed {
        BootstrapSeed {
            id: id.into(),
            ip4: ip4.into(),
            port,
            peer_id: peer.into(),
            dns4: dns4.into(),
            operator: "test".into(),
        }
    }

    const PEER: &str = "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo";

    #[test]
    fn seed_emits_the_ip_arm_and_the_dns_alias() {
        let s = seed("seed-1", "203.0.113.10", 30333, PEER, "seed-a.example.org");
        let addrs = bootstrap_seed_multiaddrs(&s);
        assert_eq!(
            addrs,
            vec![
                format!("/ip4/203.0.113.10/tcp/30333/p2p/{PEER}"),
                format!("/dns4/seed-a.example.org/tcp/30333/p2p/{PEER}"),
            ],
            "ip4 is authoritative, dns4 is an additive alias"
        );
    }

    #[test]
    fn seed_without_a_peer_id_is_never_dialed() {
        // A seed is a dial target; without an identity check a wrong/rogue host
        // would just be a wasted connection. Reject rather than guess.
        let s = seed("seed-x", "203.0.113.10", 30333, "", "seed-a.example.org");
        assert!(bootstrap_seed_multiaddrs(&s).is_empty());
    }

    #[test]
    fn seed_falls_back_to_dns_when_the_ip_is_unusable() {
        let s = seed("seed-y", "not-an-ip", 30333, PEER, "seed-a.example.org");
        assert_eq!(
            bootstrap_seed_multiaddrs(&s),
            vec![format!("/dns4/seed-a.example.org/tcp/30333/p2p/{PEER}")]
        );
        // Neither arm usable ⇒ nothing at all.
        let none = seed("seed-z", "not-an-ip", 30333, PEER, "");
        assert!(bootstrap_seed_multiaddrs(&none).is_empty());
    }

    #[test]
    fn seed_port_zero_defaults_to_30333_and_entries_dedupe() {
        let a = seed("a", "203.0.113.10", 0, PEER, "");
        let b = seed("b", "203.0.113.10", 30333, PEER, "");
        assert_eq!(
            bootstrap_seed_multiaddrs(&a),
            vec![format!("/ip4/203.0.113.10/tcp/30333/p2p/{PEER}")]
        );
        assert_eq!(
            bootstrap_seed_addrs(&[a, b]).len(),
            1,
            "the same dial target from two entries must appear once"
        );
    }

    #[test]
    fn shipped_registry_has_a_usable_floor() {
        // Parse the *embedded* registry: `load_registry()` honours an operator
        // override (`~/.gpn/network.json`), which is a different file and must
        // not be able to make this assertion vacuous.
        let registry: crate::network_registry::Registry =
            serde_json::from_str(crate::network_registry::EMBEDDED_REGISTRY)
                .expect("embedded network.json must parse");
        assert!(
            !registry.bootstrap_seeds.is_empty(),
            "network.json must ship at least one hardcoded seed (a floor of one is still a floor)"
        );
        let addrs = bootstrap_seed_addrs(&registry.bootstrap_seeds);
        assert!(!addrs.is_empty(), "shipped seeds must resolve to dialable multiaddrs");
        assert!(addrs.iter().all(|a| a.contains("/p2p/")));
        assert!(
            !registry.bootstrap_urls.is_empty(),
            "network.json must ship Source B bootstrap URLs"
        );
        assert!(registry
            .bootstrap_urls
            .iter()
            .all(|u| u.starts_with("https://")));
    }
}
