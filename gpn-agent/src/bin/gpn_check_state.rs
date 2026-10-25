//! gpn_check_state — read on-chain provider state for an account (stake + attestation).
//!
//! Item #110: this is a **diagnostic**, so it must not state a fact about the
//! chain it never read. Before, a dead socket, an unreachable node, an
//! unparseable address and an undecodable value all printed the same confident
//! `balance=0.0000 GPN … staked=false attested=false` and exited **0** — so a
//! monitor or an ops guard (the AP top-up script reads `balance=` from this
//! output) acted on a balance the chain never reported. `--help` was worse: every
//! argument was silently treated as an endpoint/address, so `--help` reported on
//! **Alice's** account.
//!
//! Exit codes (mirroring `gpn_check_feed`): 0 = the chain was read ·
//! 2 = usage error · 3 = the chain could not be read (no conclusion reached).

use gpn_agent::chain::ChainClient;

const USAGE: &str = "\
usage: gpn_check_state [endpoint] [address]

  endpoint   websocket RPC endpoint (default ws://127.0.0.1:9944)
  address    SS58 / AccountId32 to inspect
             (default 5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY)

Prints one line: account=… nonce=… balance=… GPN staked=… attested=…
Exit: 0 read ok · 2 usage · 3 chain unreadable (no conclusion was reached).
";

#[derive(Debug, PartialEq)]
pub enum Invocation {
    Help,
    Run { endpoint: String, address: String },
}

/// Resolve argv (already stripped of argv[0]) into an action.
///
/// A flag is never silently reinterpreted as an endpoint or an address: the old
/// code took `argv[1]` as the endpoint unconditionally, so `--help` connected to
/// a "node" called `--help` against Alice's account.
pub fn resolve_args(raw: &[String]) -> Result<Invocation, String> {
    if raw
        .iter()
        .any(|a| a == "--help" || a == "-h" || a == "help")
    {
        return Ok(Invocation::Help);
    }
    let mut positional = Vec::new();
    for a in raw {
        if a.starts_with('-') {
            return Err(format!("unknown option `{a}`"));
        }
        positional.push(a.clone());
    }
    if positional.len() > 2 {
        return Err(format!(
            "expected at most 2 arguments (endpoint, address), got {}",
            positional.len()
        ));
    }
    Ok(Invocation::Run {
        endpoint: positional
            .first()
            .cloned()
            .unwrap_or_else(|| "ws://127.0.0.1:9944".to_string()),
        address: positional
            .get(1)
            .cloned()
            .unwrap_or_else(|| "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY".to_string()),
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let (endpoint, address) = match resolve_args(&raw) {
        Ok(Invocation::Help) => {
            print!("{USAGE}");
            return Ok(());
        }
        Ok(Invocation::Run { endpoint, address }) => (endpoint, address),
        Err(e) => {
            eprintln!("gpn_check_state: {e}\n");
            eprint!("{USAGE}");
            std::process::exit(2);
        }
    };

    let client = ChainClient::new(&endpoint).await?;

    // The one read this tool is FOR. If it fails, say so and stop: everything
    // below is optional detail, and none of it is worth printing as fact when the
    // account itself could not be read.
    let ai = match client.get_account_info_result(&address).await {
        Ok(ai) => ai,
        Err(e) => {
            eprintln!("account={address} UNREADABLE — {e}");
            eprintln!(
                "[verdict] no conclusion about this account was reached. This is NOT a zero \
                 balance and NOT evidence the account is unfunded/not a provider; check the node \
                 at {endpoint} and the address itself."
            );
            std::process::exit(3);
        }
    };

    let nonce = match client.get_account_nonce_result(&address).await {
        Ok(n) => n.to_string(),
        Err(e) => {
            eprintln!("[warn] nonce unreadable: {e}");
            "?".to_string()
        }
    };
    let (staked, attested) = match client.get_provider_state_result(&address).await {
        Ok((s, a)) => (s.to_string(), a.to_string()),
        Err(e) => {
            eprintln!(
                "[warn] staking/attestation state unreadable: {e} — shown as `?`, not `false`"
            );
            ("?".to_string(), "?".to_string())
        }
    };

    println!(
        "account={} nonce={} balance={:.4} GPN staked={} attested={}",
        address, nonce, ai.balance, staked, attested
    );

    // Read the attestation record details (gpu_count, compute_gflops, tier).
    match client.get_attestation_record_result(&address).await {
        Ok(Some((gpus, gflops, tier))) => {
            println!("attestation: gpus={} gflops={} tier={}", gpus, gflops, tier)
        }
        Ok(None) => {}
        Err(e) => eprintln!(
            "[warn] attestation record unreadable: {e} — this is NOT evidence the account never \
             attested (no line is printed instead of a false `not attested`)"
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_flag_is_never_silently_read_as_an_endpoint_or_address() {
        // The live shape that used to report on Alice's account.
        assert_eq!(resolve_args(&v(&["--help"])), Ok(Invocation::Help));
        assert_eq!(resolve_args(&v(&["-h"])), Ok(Invocation::Help));
        assert_eq!(resolve_args(&v(&["help"])), Ok(Invocation::Help));
        assert_eq!(
            resolve_args(&v(&["ws://127.0.0.1:9944", "--help"])),
            Ok(Invocation::Help)
        );
        // An unknown flag is an error, never a positional.
        assert!(resolve_args(&v(&["--version"])).is_err());
        assert!(resolve_args(&v(&["ws://127.0.0.1:9944", "--nope"])).is_err());
    }

    #[test]
    fn defaults_and_positional_arguments_are_unchanged() {
        assert_eq!(
            resolve_args(&v(&[])),
            Ok(Invocation::Run {
                endpoint: "ws://127.0.0.1:9944".into(),
                address: "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY".into(),
            })
        );
        assert_eq!(
            resolve_args(&v(&["ws://127.0.0.1:9999"])),
            Ok(Invocation::Run {
                endpoint: "ws://127.0.0.1:9999".into(),
                address: "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY".into(),
            })
        );
        assert_eq!(
            resolve_args(&v(&[
                "ws://127.0.0.1:9999",
                "5Ft5q2AViqtBrnaZmUp86KeQDuwczo2KHze5MR99892hVd9B"
            ])),
            Ok(Invocation::Run {
                endpoint: "ws://127.0.0.1:9999".into(),
                address: "5Ft5q2AViqtBrnaZmUp86KeQDuwczo2KHze5MR99892hVd9B".into(),
            })
        );
        // More positionals than the interface defines is a usage error, not a
        // silent ignore (an ignored 3rd argument is a typo'd check that "passes").
        assert!(resolve_args(&v(&["a", "b", "c"])).is_err());
    }
}
