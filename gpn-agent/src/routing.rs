//! Client-side RTT aware routing helpers.
//!
//! The scheduler natively measures the **access-point → provider** round-trip
//! (`Provider::latency_ms`, from polling each provider's `/status` heartbeat).
//! That is *not* the metric the user actually experiences: the path a request
//! takes is **user → access point → provider**, and the user↔AP leg can dominate
//! (a user far from the AP pays that RTT on every call, regardless of which
//! provider the AP then picks).
//!
//! Item 3 of the roadmap ("Client-side RTT / user→nearest-node") closes that gap:
//! the *client* measures its own RTT to the provider endpoints (and/or the AP)
//! it is considering and passes those measurements to the access point, which
//! prefers the provider that is genuinely closest to the **user**. When no
//! client measurement is supplied we fall back to the AP-measured value, so the
//! scheduler behaves exactly as before.
//!
//! See `shared/artifacts/project7/GPN-MASTER-REFERENCE.md` §6.1 ("Latency /
//! connect to the lowest-ms node") and `design-access-points-scheduler.md`.

use std::collections::HashMap;

/// A map of client-measured RTTs (ms), keyed by provider **address** or
/// **status URL**. Both key forms are accepted and matched case-insensitively.
pub type RttOverrides = HashMap<String, u64>;

/// Normalize an override key so an address or a URL (with or without a
/// trailing slash, with or without a `/status` suffix) all compare equal.
fn normalize_key(k: &str) -> String {
    let k = k.trim().to_ascii_lowercase();
    let k = k.trim_end_matches('/');
    let k = k.strip_suffix("/status").unwrap_or(k);
    k.trim_end_matches('/').to_string()
}

/// Resolve the effective latency to use when scoring a provider.
///
/// Preference order:
///   1. a client-measured (user→provider) RTT, when supplied and non-zero;
///   2. the access-point-measured latency otherwise.
///
/// A `0` override is treated as "not measured" (a client that couldn't reach the
/// endpoint reports 0, not a spuriously perfect latency).
pub fn effective_latency_ms(ap_ms: u64, client_ms: Option<u64>) -> u64 {
    match client_ms {
        Some(m) if m > 0 => m,
        _ => ap_ms,
    }
}

/// Build an [`RttOverrides`] map from a JSON object of `{ "<key>": <ms> }`.
///
/// Non-numeric, negative, or unparseable entries are ignored; keys are stored
/// normalized (see [`normalize_key`]). This is deliberately lenient: a malformed
/// RTT hint must never reject or fail a job — it just falls back to the
/// AP-measured latency.
pub fn parse_rtt_overrides(v: &serde_json::Value) -> RttOverrides {
    let mut out = RttOverrides::new();
    if let Some(obj) = v.as_object() {
        for (k, val) in obj {
            // Skip non-positive / unparseable entries: they mean "not measured"
            // (see `effective_latency_ms`), so they must not create a spurious
            // perfect-latency override.
            let ms = if let Some(ms) = val.as_u64() {
                ms
            } else if let Some(ms) = val.as_f64() {
                if ms > 0.0 {
                    ms.round() as u64
                } else {
                    continue;
                }
            } else {
                continue;
            };
            if ms > 0 {
                out.insert(normalize_key(k), ms);
            }
        }
    }
    out
}

/// Look up a client-measured RTT for a provider by its address or status URL.
pub fn lookup_override(o: &RttOverrides, address: &str, url: &str) -> Option<u64> {
    if !address.is_empty() {
        if let Some(ms) = o.get(&normalize_key(address)) {
            return Some(*ms);
        }
    }
    if !url.is_empty() {
        if let Some(ms) = o.get(&normalize_key(url)) {
            return Some(*ms);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn client_measurement_wins_over_ap_measurement() {
        // Client says the provider is 40ms from the *user*; the AP measures 5ms.
        // The user-relevant number is 40 (that's the path they pay).
        assert_eq!(effective_latency_ms(5, Some(40)), 40);
        assert_eq!(effective_latency_ms(5, None), 5);
    }

    #[test]
    fn zero_override_is_treated_as_unmeasured() {
        assert_eq!(effective_latency_ms(120, Some(0)), 120);
        assert_eq!(effective_latency_ms(120, None), 120);
    }

    #[test]
    fn keys_match_by_address_or_url_regardless_of_shape() {
        let v = json!({
            "http://gpn-testnet.duckdns.org:8093/": 30,
            "5EnqSp5QexampleAddress": 12.7
        });
        let o = parse_rtt_overrides(&v);

        // URL key: with `/status`, trailing slash, and uppercase all normalize.
        assert_eq!(lookup_override(&o, "", "http://GPN-testnet.duckdns.org:8093/status"), Some(30));
        assert_eq!(lookup_override(&o, "", "http://gpn-testnet.duckdns.org:8093"), Some(30));

        // Address key, case-insensitive; float rounds to nearest ms.
        assert_eq!(lookup_override(&o, "5enqsp5qEXAMPLEaddress", ""), Some(13));

        // Unknown provider → no override → falls back to AP value.
        assert_eq!(lookup_override(&o, "5Nobody", "http://elsewhere:1"), None);
    }

    #[test]
    fn malformed_entries_are_ignored_not_fatal() {
        let v = json!({"a": "not-a-number", "b": -3, "c": 0, "d": 7});
        let o = parse_rtt_overrides(&v);
        assert_eq!(o.len(), 1);
        assert_eq!(o.get("d"), Some(&7));
        // Non-object input yields an empty map rather than erroring.
        assert!(parse_rtt_overrides(&json!([1, 2, 3])).is_empty());
    }

    #[test]
    fn choosing_lowest_user_rtt_reorders_ap_view() {
        // AP sees provider A as nearer (5ms) and B as farther (50ms), so the
        // default (AP-only) pick is A. But the *user* is closer to B (client=10
        // vs A client=80). With client RTT supplied, B must win.
        let ap = |a: u64, b: u64| -> (u64, u64) {
            // score in the *user* frame: min effective latency
            (effective_latency_ms(a, Some(80)), effective_latency_ms(b, Some(10)))
        };
        let (a, b) = ap(5, 50);
        assert!(b < a, "client-nearest provider (B) must score lower than A");
        assert_eq!((a, b), (80, 10));
    }
}
