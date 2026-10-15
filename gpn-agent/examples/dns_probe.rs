//! dns_probe — reproduce the "host-DNS /result defect" and prove the fix.
//!
//! Times a dual-stack `reqwest` GET (default AF_UNSPEC resolver) against an
//! IPv4-only GET (the `net::ipv4_only_client` used by the provider `/result`
//! callback) for a public `*.duckdns.org` URL. On a host whose resolver does not
//! answer AAAA, the dual-stack leg stalls and returns an error, while the
//! IPv4-only leg succeeds immediately — with no host-DNS change required.
//!
//! Run: `cargo run --example dns_probe -- http://gpn-testnet.duckdns.org:8092/health`

use std::time::{Duration, Instant};

fn time_get(label: &str, client: reqwest::blocking::Client, url: &str) {
    let t = Instant::now();
    match client.get(url).send() {
        Ok(r) => println!(
            "{label:12} http={} in {:.3}s",
            r.status().as_u16(),
            t.elapsed().as_secs_f64()
        ),
        Err(e) => println!("{label:12} ERROR in {:.3}s: {e}", t.elapsed().as_secs_f64()),
    }
}

fn main() {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "http://gpn-testnet.duckdns.org:8092/health".to_string());
    println!("probing {url}");

    let default = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .unwrap();
    time_get("dual-stack", default, &url);

    let v4 = gpn_agent::net::ipv4_only_client(Duration::from_secs(8)).unwrap();
    time_get("ipv4-only", v4, &url);
}
