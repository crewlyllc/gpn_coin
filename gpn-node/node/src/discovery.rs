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
//!    peer-list JSON.
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

use sc_network::config::MultiaddrWithPeerId;
use sc_network::Multiaddr;
use std::path::Path;
use std::time::Duration;

/// Filename (under the node's base path) of the persisted peer store.
pub const PEERS_FILE: &str = "gpn-peers.json";

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

/// Fetch and parse a published seed-list JSON over HTTP(S).
///
/// Note: the node is built without TLS (see `Cargo.toml`), so `https://` URLs
/// fail with a clear error that is logged and ignored — startup is unaffected.
/// Prefer plain `http://` seed sources reachable from the node.
pub async fn fetch_seed_url(url: &str) -> Result<Vec<String>, String> {
    let url = url.to_string();
    let text = tokio::task::spawn_blocking(move || {
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
    .map_err(|e| format!("join error: {e}"))??;
    Ok(parse_seed_payload(&text))
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
}
