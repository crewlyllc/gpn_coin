//! `gpn-dialcheck` — verify one public multiaddr by completing a libp2p
//! handshake, and say who actually answers.
//!
//! This is the only tool in GPN that may assert a public address is reachable.
//! Everything else (the provider status ladder, the access point, the
//! publisher) consumes its answer.
//!
//! ```text
//! gpn-dialcheck /ip4/203.0.113.7/tcp/30333/p2p/12D3KooW…
//!   exit 0  verified      the handshake returned the expected peer id
//!   exit 2  wrong-peer    a *different* peer id answered (prints both)
//!   exit 3  unreachable   no handshake (timeout / refused / protocol error)
//!   exit 4  skipped       verifier shares the target's egress (never a pass)
//!   exit 1  usage / bad arguments
//! ```
//!
//! `--egress <ip>` supplies the verifier's own public IP; when it equals the
//! target's host the check is skipped rather than falsely passed. `--json`
//! prints the machine-readable outcome.
//!
//! Why this exists: on 2026-11-12 two machines behind one NAT both published
//! `/ip4/142.129.54.88/tcp/30333`, each under its own peer id, and every client
//! that merged them died on `DuplicateBootnode`. A TCP connect succeeds for
//! both; a handshake succeeds for exactly one. This command is that handshake.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 || args[1] == "-h" || args[1] == "--help" {
        eprintln!(
            "usage: gpn-dialcheck <multiaddr-with-/p2p/<id>> [--timeout-secs N] [--egress IP] [--json]"
        );
        std::process::exit(1);
    }
    let target = args[1].clone();
    let mut timeout_secs: u64 = 5;
    let mut egress: Option<String> = None;
    let mut json = false;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--timeout-secs" if i + 1 < args.len() => {
                timeout_secs = args[i + 1].parse().unwrap_or(5).max(1);
                i += 2;
            }
            "--egress" if i + 1 < args.len() => {
                egress = Some(args[i + 1].clone());
                i += 2;
            }
            "--json" => {
                json = true;
                i += 1;
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(1);
            }
        }
    }
    // The expected peer id is the address's /p2p tail — that is the claim
    // being tested. A missing tail is a usage error, not an "unreachable".
    let expected = match gpn_agent::dialcheck::peer_id_of(&target) {
        Some(p) => p,
        None => {
            eprintln!("error: {target} has no /p2p/<peer-id> suffix");
            std::process::exit(1);
        }
    };
    let out = gpn_agent::dialcheck::verify(
        &target,
        &expected,
        egress.as_deref(),
        std::time::Duration::from_secs(timeout_secs),
    );
    if json {
        println!(
            "{}",
            serde_json::json!({
                "target": target,
                "expected_peer_id": expected,
                "outcome": out.code(),
                "verified": out.is_verified(),
                "detail": out.summary(),
            })
        );
    } else {
        println!("{}: {}", out.code(), out.summary());
    }
    std::process::exit(out.exit_code());
}
