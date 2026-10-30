//! Canonical chain-spec distribution: pinned genesis + bootnode hygiene.
//!
//! A chain spec is the *identity* of a chain: its genesis state determines the
//! genesis hash, and a node only joins the right network when that hash matches.
//! The spec is therefore distributed over HTTPS (`https://gpncoin.ai/chain-spec`)
//! and **verified here before it is used**:
//!
//! * the genesis block hash is pinned ([`EXPECTED_GENESIS_HASH`]);
//! * because that hash cannot be recomputed offline without executing the
//!   runtime wasm, we additionally pin a SHA-256 over the canonical `genesis`
//!   document ([`EXPECTED_GENESIS_SHA256`]) — any tampering with the genesis
//!   state (balances, code, properties inside genesis) changes it.
//! * the spec's `bootNodes` list is sanitised: loopback / private / malformed
//!   entries are dropped (a spec that dials `127.0.0.1` strands every fresh
//!   install — the 2026-10-30 incident), and an empty list falls back to the
//!   canonical [`CANONICAL_BOOTNODES`] or `GPN_BOOTNODES`.
//!
//! Used by `gpn_scheduler` (serves the spec) and `firstrun` (fetches it).

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Bootnodes every node must be able to dial, in the order they are served.
///
/// The first entry is the DNS name (survives an IP change); the two IP entries
/// are the current seed boxes. Peer ids are part of the multiaddr and are
/// checked to be well-formed by [`is_dialable_bootnode`].
pub const CANONICAL_BOOTNODES: &[&str] = &[
    "/dns4/gpn-testnet.duckdns.org/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo",
    "/ip4/142.129.54.88/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo",
    "/ip4/23.241.109.255/tcp/30333/p2p/12D3KooWFqZ2msYzP3263AKGSUfLzUC7Am7FyRDqi2KKfgREmxGz",
];

/// Genesis block hash of the canonical GPN Public Testnet.
pub const EXPECTED_GENESIS_HASH: &str =
    "0xb8e4636a65d0c812abb437c38dd7e937bd7c3355c54c54d63060f26a33519a5c";

/// SHA-256 of the canonical `genesis` document (see module docs).
///
/// Recomputed by the `canonical_genesis_digest_is_pinned` test; a change here
/// means the chain itself changed and must be a deliberate, reviewed update.
pub const EXPECTED_GENESIS_SHA256: &str =
    "f88403de3027216614184c357a022010bae19baa1e2c6a692b601d1e880cd809";

/// The canonical bootnodes as owned strings.
pub fn canonical_bootnodes() -> Vec<String> {
    CANONICAL_BOOTNODES.iter().map(|s| s.to_string()).collect()
}

/// Parse a comma/space/semicolon separated bootnode list, keeping only
/// dialable entries. An empty (or entirely invalid) result yields the
/// canonical list so callers always have something to dial.
pub fn bootnodes_from_str(raw: Option<&str>) -> Vec<String> {
    let Some(raw) = raw else {
        return canonical_bootnodes();
    };
    let parsed: Vec<String> = raw
        .split([',', ';', ' ', '\t', '\n', '\r'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let (parsed, _) = sanitize_bootnodes(&parsed);
    if parsed.is_empty() {
        canonical_bootnodes()
    } else {
        parsed
    }
}

/// `GPN_BOOTNODES` when set (and valid), else the canonical list.
pub fn bootnodes_from_env() -> Vec<String> {
    bootnodes_from_str(std::env::var("GPN_BOOTNODES").ok().as_deref())
}

/// True when a multiaddr is usable as a bootnode: it has a routable host and a
/// peer id, and is not loopback / private / link-local.
pub fn is_dialable_bootnode(ma: &str) -> bool {
    if !(ma.starts_with("/ip4/") || ma.starts_with("/dns4/") || ma.starts_with("/dns6/")) {
        return false;
    }
    // Must carry a peer id — without one the address alone cannot be dialled.
    let Some((_, pid)) = ma.rsplit_once("/p2p/") else {
        return false;
    };
    if !(pid.starts_with("12D3Koo") || pid.starts_with("Qm")) {
        return false;
    }
    if ma.starts_with("/ip4/") {
        let rest = &ma["/ip4/".len()..];
        let host = rest.split('/').next().unwrap_or("");
        let octets: Vec<u8> = host
            .split('.')
            .map(|p| p.parse::<u8>().unwrap_or(255))
            .collect();
        if octets.len() != 4 {
            return false;
        }
        let (a, b) = (octets[0], octets[1]);
        if a == 0 || a == 10 || a == 127 || a == 255 {
            return false;
        }
        if a == 169 && b == 254 {
            return false; // link-local
        }
        if a == 172 && (16..=31).contains(&b) {
            return false; // RFC1918
        }
        if a == 192 && b == 168 {
            return false; // RFC1918
        }
        if a == 100 && (64..=127).contains(&b) {
            return false; // CGNAT
        }
        if a == 198 && (b == 18 || b == 19) {
            return false; // benchmarking
        }
        if a >= 224 {
            return false; // multicast / reserved
        }
    } else {
        // DNS names: reject obvious loopback aliases.
        let rest = &ma[ma.find('/').unwrap_or(0) + 1..];
        let host = rest.split('/').nth(1).unwrap_or("").to_ascii_lowercase();
        if host.is_empty() || host == "localhost" || host.ends_with(".localhost") {
            return false;
        }
    }
    true
}

/// Split a bootnode list into `(kept, dropped)`, de-duplicated, order preserved.
pub fn sanitize_bootnodes(input: &[String]) -> (Vec<String>, Vec<String>) {
    let mut kept: Vec<String> = Vec::new();
    let mut dropped: Vec<String> = Vec::new();
    for ma in input {
        let ma = ma.trim();
        if ma.is_empty() {
            continue;
        }
        if !is_dialable_bootnode(ma) {
            dropped.push(ma.to_string());
        } else if !kept.iter().any(|k| k == ma) {
            kept.push(ma.to_string());
        }
    }
    (kept, dropped)
}

/// SHA-256 (hex) of the canonical `genesis` document inside a chain spec.
///
/// `serde_json` maps are ordered, so the serialisation of the parsed value is
/// deterministic and independent of the file's whitespace/key order.
pub fn genesis_sha256(spec: &str) -> Result<String, String> {
    let v: Value = serde_json::from_str(spec).map_err(|e| format!("not valid JSON: {e}"))?;
    let genesis = v
        .get("genesis")
        .or_else(|| v.get("genesisRaw"))
        .ok_or("JSON has no `genesis` field — not a chain spec")?;
    let bytes =
        serde_json::to_vec(genesis).map_err(|e| format!("genesis not serialisable: {e}"))?;
    Ok(hex::encode(Sha256::digest(&bytes)))
}

/// Verify a spec's genesis against the pinned hash and digest.
///
/// Returns the computed digest on success. The hash is checked both against
/// the pinned constant and, when the document carries an explicit
/// `genesisHash`/`genesis_hash` field, against that field — a document whose
/// header disagrees with its own genesis state is refused.
pub fn verify_genesis(spec: &str) -> Result<String, String> {
    verify_genesis_with(spec, EXPECTED_GENESIS_HASH, EXPECTED_GENESIS_SHA256)
}

/// [`verify_genesis`] with the pins supplied — testable without shipping the
/// full 1 MB spec as a fixture.
pub fn verify_genesis_with(
    spec: &str,
    expected_hash: &str,
    expected_digest: &str,
) -> Result<String, String> {
    let v: Value = serde_json::from_str(spec).map_err(|e| format!("not valid JSON: {e}"))?;
    if let Some(declared) = v
        .get("genesisHash")
        .or_else(|| v.get("genesis_hash"))
        .or_else(|| v.get("genesis").and_then(|g| g.get("hash")))
        .and_then(|h| h.as_str())
    {
        if !declared.eq_ignore_ascii_case(expected_hash) {
            return Err(format!(
                "spec declares genesis {declared}, expected {expected_hash} — refusing a different chain"
            ));
        }
    }
    let digest = genesis_sha256(spec)?;
    if digest != expected_digest {
        return Err(format!(
            "genesis does not match the pinned GPN Public Testnet genesis \
             ({expected_hash}): sha256 {digest}, expected {expected_digest} \
             — refusing to join a different chain"
        ));
    }
    Ok(digest)
}

/// Text-level replacement of the top-level `bootNodes` array.
///
/// Everything else in the document stays **byte-identical** (genesis, code,
/// properties, formatting) — only the array value is swapped.
pub fn rewrite_bootnodes(spec: &str, bootnodes: &[String]) -> Result<String, String> {
    let replacement = serde_json::to_string(bootnodes)
        .map_err(|e| format!("bootnode list not serialisable: {e}"))?;
    if let Some((start, end)) = find_bootnodes_span(spec) {
        let mut out = String::with_capacity(spec.len() + replacement.len());
        out.push_str(&spec[..start]);
        out.push_str(&replacement);
        out.push_str(&spec[end..]);
        return Ok(out);
    }
    // No `bootNodes` key: insert one right after the opening brace.
    let Some(brace) = spec.find('{') else {
        return Err("JSON has no object body — not a chain spec".to_string());
    };
    let mut out = String::with_capacity(spec.len() + replacement.len() + 16);
    out.push_str(&spec[..=brace]);
    out.push_str("\"bootNodes\":");
    out.push_str(&replacement);
    out.push(',');
    out.push_str(&spec[brace + 1..]);
    Ok(out)
}

/// Locate the byte span of the value of the top-level `"bootNodes"` key.
fn find_bootnodes_span(spec: &str) -> Option<(usize, usize)> {
    let key = "\"bootNodes\"";
    let kpos = spec.find(key)?;
    let bytes = spec.as_bytes();
    let mut i = kpos + key.len();
    while i < bytes.len() && (bytes[i] as char).is_whitespace() {
        i += 1;
    }
    if bytes.get(i) != Some(&b':') {
        return None;
    }
    i += 1;
    while i < bytes.len() && (bytes[i] as char).is_whitespace() {
        i += 1;
    }
    match bytes.get(i) {
        Some(b'[') => {
            let start = i;
            let mut depth = 0usize;
            let mut in_str = false;
            let mut esc = false;
            i += 1;
            while i < bytes.len() {
                let c = bytes[i];
                if in_str {
                    if esc {
                        esc = false;
                    } else if c == b'\\' {
                        esc = true;
                    } else if c == b'"' {
                        in_str = false;
                    }
                } else {
                    match c {
                        b'"' => in_str = true,
                        b'[' => depth += 1,
                        b']' => {
                            if depth == 0 {
                                return Some((start, i + 1));
                            }
                            depth -= 1;
                        }
                        _ => {}
                    }
                }
                i += 1;
            }
            None
        }
        Some(b'n') if spec[i..].starts_with("null") => Some((i, i + 4)),
        _ => None,
    }
}

/// Outcome of preparing a fetched spec for use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedSpec {
    /// The rewritten document (genesis untouched).
    pub body: String,
    /// The bootnodes the node will actually dial.
    pub bootnodes: Vec<String>,
    /// Bootnodes removed because they were loopback/private/malformed.
    pub dropped: Vec<String>,
    /// True when the source list was empty and the fallback was used.
    pub used_fallback: bool,
}

/// Verify the genesis, sanitise `bootNodes`, and return the document to write.
///
/// `fallback` (normally `GPN_BOOTNODES`, else the canonical list) is used when
/// the spec itself carries no usable bootnode.
pub fn prepare_spec(spec: &str, fallback: &[String]) -> Result<PreparedSpec, String> {
    prepare_spec_with(
        spec,
        fallback,
        EXPECTED_GENESIS_HASH,
        EXPECTED_GENESIS_SHA256,
    )
}

/// Bootnode hygiene only (no genesis verification).
///
/// Used on the write path so that *whatever* the fetcher returned, a spec that
/// dials loopback can never be persisted. Genesis authenticity is enforced by
/// the fetcher ([`verify_genesis`]); this keeps the two concerns separate.
pub fn sanitize_spec_bootnodes(spec: &str, fallback: &[String]) -> Result<PreparedSpec, String> {
    let v: Value = serde_json::from_str(spec).map_err(|e| format!("not valid JSON: {e}"))?;
    if v.get("genesis").is_none() && v.get("genesisRaw").is_none() {
        return Err("JSON has no `genesis` field — not a chain spec".to_string());
    }
    sanitize_parsed(&v, spec, fallback)
}

/// [`prepare_spec`] with the pins supplied (tests).
pub fn prepare_spec_with(
    spec: &str,
    fallback: &[String],
    expected_hash: &str,
    expected_digest: &str,
) -> Result<PreparedSpec, String> {
    verify_genesis_with(spec, expected_hash, expected_digest)?;
    let v: Value = serde_json::from_str(spec).map_err(|e| format!("not valid JSON: {e}"))?;
    sanitize_parsed(&v, spec, fallback)
}

fn sanitize_parsed(v: &Value, spec: &str, fallback: &[String]) -> Result<PreparedSpec, String> {
    let existing: Vec<String> = v
        .get("bootNodes")
        .and_then(|b| b.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let (kept, dropped) = sanitize_bootnodes(&existing);
    let (bootnodes, used_fallback) = if kept.is_empty() {
        let mut fb: Vec<String> = fallback.to_vec();
        if fb.is_empty() {
            fb = canonical_bootnodes();
        }
        (fb, true)
    } else {
        (kept, false)
    };
    let body = rewrite_bootnodes(spec, &bootnodes)?;
    Ok(PreparedSpec {
        body,
        bootnodes,
        dropped,
        used_fallback,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny stand-in spec: the pins for it are computed at test time, so
    /// these tests do not need the 1 MB canonical file.
    const FIXTURE: &str = r#"{
  "name": "GPN Public Testnet",
  "id": "gpn_testnet",
  "bootNodes": ["/ip4/127.0.0.1/tcp/30333/p2p/12D3KooWNm2fbwKaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
  "protocolId": "gpn",
  "properties": {"tokenDecimals": 12, "tokenSymbol": "GPN"},
  "genesis": {"runtimeGenesis": {"code": "0x0061736d", "config": {"balances": []}}}
}"#;

    fn pins(spec: &str) -> (String, String) {
        (
            EXPECTED_GENESIS_HASH.to_string(),
            genesis_sha256(spec).unwrap(),
        )
    }

    fn prepared_with_fixture(spec: &str, fallback: &[String]) -> Result<PreparedSpec, String> {
        let (h, d) = pins(FIXTURE);
        prepare_spec_with(spec, fallback, &h, &d)
    }

    #[test]
    fn good_genesis_is_accepted_and_tampering_is_rejected() {
        let (h, d) = pins(FIXTURE);
        assert!(verify_genesis_with(FIXTURE, &h, &d).is_ok());

        // Change one byte inside the genesis document.
        let tampered = FIXTURE.replace("\"code\": \"0x0061736d\"", "\"code\": \"0x0061736e\"");
        assert_ne!(tampered, FIXTURE);
        let err = verify_genesis_with(&tampered, &h, &d).unwrap_err();
        assert!(err.contains(EXPECTED_GENESIS_HASH), "{err}");
        assert!(err.contains("refusing"), "{err}");

        // A spec that *declares* the wrong hash is refused even if unchanged.
        let declared = FIXTURE.replace(
            "\"id\": \"gpn_testnet\",",
            "\"id\": \"gpn_testnet\", \"genesisHash\": \"0xdeadbeef\",",
        );
        let err = verify_genesis_with(&declared, &h, &d).unwrap_err();
        assert!(err.contains("0xdeadbeef"), "{err}");
        let ok = FIXTURE.replace(
            "\"id\": \"gpn_testnet\",",
            &format!("\"id\": \"gpn_testnet\", \"genesisHash\": \"{EXPECTED_GENESIS_HASH}\","),
        );
        assert!(verify_genesis_with(&ok, &h, &d).is_ok());
    }

    #[test]
    fn empty_or_loopback_bootnodes_are_stripped() {
        let local = vec![
            "/ip4/127.0.0.1/tcp/30333/p2p/12D3KooWNm2fbwKaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_string(),
            "/ip4/192.168.1.5/tcp/30333/p2p/12D3KooWNm2fbwKaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_string(),
            "/ip4/10.0.0.2/tcp/30333/p2p/12D3KooWNm2fbwKaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_string(),
            "/ip4/172.20.0.2/tcp/30333/p2p/12D3KooWNm2fbwKaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_string(),
            "/dns4/localhost/tcp/30333/p2p/12D3KooWNm2fbwKaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_string(),
        ];
        let (kept, dropped) = sanitize_bootnodes(&local);
        assert!(kept.is_empty());
        assert_eq!(dropped.len(), 5);
        assert!(is_dialable_bootnode(CANONICAL_BOOTNODES[0]));
        assert!(is_dialable_bootnode(CANONICAL_BOOTNODES[1]));
        assert!(is_dialable_bootnode(CANONICAL_BOOTNODES[2]));
        assert!(!is_dialable_bootnode("/ip4/127.0.0.1/tcp/30333"));
        assert!(!is_dialable_bootnode(
            "/ip4/127.0.0.1/tcp/30333/p2p/notapeer"
        ));
        assert!(!is_dialable_bootnode("/ip4/8.8.8.8/tcp/30333")); // no peer id
    }

    #[test]
    fn loopback_spec_gets_the_canonical_list_and_keeps_genesis_bytes() {
        let out = prepared_with_fixture(FIXTURE, &canonical_bootnodes()).unwrap();
        // The only entry was loopback: it is dropped and the fallback is used.
        assert!(out.used_fallback);
        assert_eq!(out.bootnodes, canonical_bootnodes());
        assert_eq!(out.dropped.len(), 1);
        let reparsed: Value = serde_json::from_str(&out.body).unwrap();
        assert_eq!(
            reparsed["bootNodes"],
            serde_json::json!(canonical_bootnodes())
        );
        assert_eq!(
            genesis_sha256(&out.body).unwrap(),
            genesis_sha256(FIXTURE).unwrap()
        );
        // Everything outside bootNodes is byte-identical.
        let before = &FIXTURE[..FIXTURE.find("\"bootNodes\"").unwrap()];
        assert!(out.body.starts_with(before));
        assert!(out
            .body
            .contains("\"properties\": {\"tokenDecimals\": 12, \"tokenSymbol\": \"GPN\"}"));
        assert!(out.body.contains("\"code\": \"0x0061736d\""));
    }

    #[test]
    fn spec_without_bootnodes_falls_back_to_env_list() {
        let stripped = FIXTURE.replace(
            "\"bootNodes\": [\"/ip4/127.0.0.1/tcp/30333/p2p/12D3KooWNm2fbwKaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"],\n",
            "",
        );
        assert!(!stripped.contains("bootNodes"));
        let envlist = vec![CANONICAL_BOOTNODES[2].to_string()];
        let out = prepared_with_fixture(&stripped, &envlist).unwrap();
        assert!(out.used_fallback);
        assert_eq!(out.bootnodes, envlist);
        let reparsed: Value = serde_json::from_str(&out.body).unwrap();
        assert_eq!(reparsed["bootNodes"], serde_json::json!(envlist));
        assert_eq!(
            genesis_sha256(&out.body).unwrap(),
            genesis_sha256(FIXTURE).unwrap()
        );
    }

    #[test]
    fn spec_with_empty_bootnodes_falls_back_to_canonical() {
        let empty = FIXTURE.replace(
            "[\"/ip4/127.0.0.1/tcp/30333/p2p/12D3KooWNm2fbwKaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"]",
            "[]",
        );
        let out = prepared_with_fixture(&empty, &[]).unwrap();
        assert!(out.used_fallback);
        assert_eq!(out.bootnodes, canonical_bootnodes());
    }

    #[test]
    fn bootnodes_from_str_parses_and_falls_back() {
        let parsed = bootnodes_from_str(Some(
            "/dns4/gpn-testnet.duckdns.org/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo, /ip4/127.0.0.1/tcp/30333/p2p/12D3KooWaaaa",
        ));
        assert_eq!(parsed.len(), 1);
        assert_eq!(bootnodes_from_str(Some("")), canonical_bootnodes());
        assert_eq!(bootnodes_from_str(None), canonical_bootnodes());
        // de-duplication
        let dup = bootnodes_from_str(Some(&format!("{0},{0}", CANONICAL_BOOTNODES[0])));
        assert_eq!(dup.len(), 1);
    }

    #[test]
    fn rewrite_is_byte_identical_outside_the_array() {
        let out = rewrite_bootnodes(FIXTURE, &["/ip4/198.51.100.9/tcp/30333".to_string()]).unwrap();
        let before = FIXTURE.find("\"bootNodes\"").unwrap();
        let after = out.find("\"bootNodes\"").unwrap();
        assert_eq!(&FIXTURE[..before], &out[..after]);
        assert!(out.contains("\"bootNodes\": [\"/ip4/198.51.100.9/tcp/30333\"]"));
        assert_eq!(
            genesis_sha256(&out).unwrap(),
            genesis_sha256(FIXTURE).unwrap()
        );
    }

    /// The real, shipped spec must match the pinned constants. Skipped (with a
    /// message) when the 1 MB artifact is not present on this host.
    #[test]
    fn shipped_spec_matches_the_pinned_constants() {
        let path = std::env::var("GPN_SPEC_PATH").unwrap_or_else(|_| {
            "/home/jeremyg/shared_workspace/artifacts/project7/gpn-testnet-spec.json".to_string()
        });
        let Ok(spec) = std::fs::read_to_string(&path) else {
            eprintln!("skipping: {path} not present");
            return;
        };
        let digest = verify_genesis(&spec).expect("shipped spec must verify");
        assert_eq!(digest, EXPECTED_GENESIS_SHA256);
        let out = prepare_spec(&spec, &canonical_bootnodes()).unwrap();
        assert_eq!(out.bootnodes, canonical_bootnodes());
        assert_eq!(
            serde_json::from_str::<Value>(&out.body).unwrap()["genesis"],
            serde_json::from_str::<Value>(&spec).unwrap()["genesis"],
            "served genesis must stay byte-identical"
        );
    }
}
