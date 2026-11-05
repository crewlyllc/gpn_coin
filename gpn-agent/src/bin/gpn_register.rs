//! gpn_register — register this machine as a GPU provider.
//!
//! Two modes:
//!
//! * **`--entry`** — first-time onboarding with **no coins**: mine a
//!   proof-of-work entry against the chain's live difficulty, then submit
//!   `ComputeMarketplace::register_provider_entry(pow, …)` — one call that
//!   proves the work, records the attestation and registers the provider. This
//!   is the path a zero-balance node needs.
//! * **default (no `--entry`)** — the legacy paid path: `register_provider`.
//!   It still needs the account to exist/fund the fee, exactly as before.
//!
//! Usage:
//!   gpn_register --entry [--endpoint ws://127.0.0.1:9944] [--signer //Alice]
//!                        [--gflops N] [--gpu-hash HEX32] [--gpu-count N]
//!   gpn_register [endpoint] [signer_uri]           (legacy paid registration)
//!
//! Idempotent: an already-registered account exits 0 with "already registered"
//! and never mines.

use gpn_agent::firstrun;
use gpn_agent::onboard::EntryStatus;
use gpn_agent::onboard::{
    self, EntryOutcome, EntryParams, LiveEntryChain, ProviderRequirements, SharedSealer,
};
use std::path::PathBuf;

struct Args {
    entry: bool,
    endpoint: String,
    signer: Option<String>,
    keystore: Option<PathBuf>,
    gflops: Option<u64>,
    gpu_hash: Option<[u8; 32]>,
    gpu_count: Option<u8>,
    positional: Vec<String>,
}

fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    let s = s.trim().trim_start_matches("0x");
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut a = Args {
        entry: false,
        endpoint: String::new(),
        signer: None,
        keystore: None,
        gflops: None,
        gpu_hash: None,
        gpu_count: None,
        positional: Vec::new(),
    };
    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        let take = |name: &str| -> Result<String, String> {
            argv.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{name} requires a value"))
        };
        match arg.as_str() {
            "--entry" => a.entry = true,
            "--endpoint" => {
                a.endpoint = take("--endpoint")?;
                i += 1;
            }
            "--signer" | "--signer-uri" => {
                a.signer = Some(take("--signer")?);
                i += 1;
            }
            "--keystore" => {
                a.keystore = Some(PathBuf::from(take("--keystore")?));
                i += 1;
            }
            "--gflops" => {
                a.gflops = Some(
                    take("--gflops")?
                        .parse()
                        .map_err(|_| "--gflops must be a number".to_string())?,
                );
                i += 1;
            }
            "--gpu-count" => {
                a.gpu_count = Some(
                    take("--gpu-count")?
                        .parse()
                        .map_err(|_| "--gpu-count must be a number".to_string())?,
                );
                i += 1;
            }
            "--gpu-hash" => {
                let v = take("--gpu-hash")?;
                a.gpu_hash = Some(
                    parse_hex32(&v)
                        .ok_or_else(|| "--gpu-hash must be 32 bytes of hex".to_string())?,
                );
                i += 1;
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown flag {other}"));
            }
            other => a.positional.push(other.to_string()),
        }
        i += 1;
    }
    Ok(a)
}

/// Resolve the signing key: explicit `--signer`, then env, then the node's
/// keystore (headless machines have no TTY to type a mnemonic into).
fn resolve_signer(a: &Args) -> Result<(String, String), String> {
    if let Some(s) = &a.signer {
        return Ok((s.clone(), "--signer".into()));
    }
    for key in ["GPN_SIGNER", "GPN_SIGNER_URI"] {
        if let Ok(v) = std::env::var(key) {
            if !v.trim().is_empty() {
                return Ok((v, key.into()));
            }
        }
    }
    let path = a.keystore.clone().or_else(|| {
        std::env::var("GPN_KEYSTORE")
            .ok()
            .map(PathBuf::from)
            .or_else(|| {
                Some(firstrun::default_keystore_path(
                    &firstrun::default_base_path(),
                ))
            })
    });
    if let Some(p) = path {
        if p.is_file() {
            let secret = firstrun::secret_from_keystore(&p)
                .map_err(|e| format!("could not read keystore {}: {e}", p.display()))?;
            return Ok((secret, format!("keystore {}", p.display())));
        }
    }
    Ok(("//Alice".into(), "default //Alice".into()))
}

#[tokio::main]
async fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.iter().any(|a| a == "--help" || a == "-h") {
        println!("gpn_register — register this machine as a GPU provider");
        println!();
        println!("  gpn_register --entry [options]   # free in coins: proof-of-work entry");
        println!("  gpn_register [endpoint] [signer] # paid registration (legacy)");
        println!();
        println!("Options:");
        println!("  --endpoint <url>    chain RPC (default ws://127.0.0.1:9944)");
        println!("  --signer <uri>      signer URI/seed (default: env, keystore, //Alice)");
        println!("  --keystore <path>   sr25519 JSON keystore to sign with");
        println!("  --gflops <n>        skip the FP32 benchmark and use this value");
        println!("  --gpu-hash <hex32>  skip the GPU probe and use this fingerprint");
        println!("  --gpu-count <n>     number of GPUs to attest (default: detected)");
        std::process::exit(0);
    }
    let args = match parse_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("gpn_register: {e}");
            eprintln!("usage: gpn_register --entry [--endpoint URL] [--signer URI] [--gflops N] [--gpu-hash HEX32]");
            std::process::exit(2);
        }
    };

    // Legacy positional form: `gpn_register [endpoint] [signer_uri]`.
    let endpoint = if !args.endpoint.is_empty() {
        args.endpoint.clone()
    } else if let Some(p) = args.positional.first() {
        p.clone()
    } else {
        std::env::var("GPN_ENDPOINT").unwrap_or_else(|_| "ws://127.0.0.1:9944".into())
    };
    let signer_uri = match resolve_signer(&args) {
        Ok((s, _)) => s,
        Err(e) => {
            eprintln!("gpn_register: {e}");
            std::process::exit(2);
        }
    };

    println!("== gpn_register ==");
    println!("  endpoint: {endpoint}");
    println!(
        "  mode:     {}",
        if args.entry {
            "proof-of-work entry (free in coins)"
        } else {
            "paid registration (legacy)"
        }
    );

    let chain = match LiveEntryChain::connect(&endpoint, &signer_uri).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("gpn_register: cannot connect to {endpoint}: {e}");
            std::process::exit(2);
        }
    };
    let account = match chain.account() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("gpn_register: {e}");
            std::process::exit(2);
        }
    };
    println!("  account:  {account}");

    // Pre-read so we can short-circuit *before* touching the GPU, and so the
    // attestation challenge is the live one.
    let state = match chain.state(&account).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("gpn_register: could not read chain state: {e}");
            std::process::exit(2);
        }
    };
    let status = EntryStatus::from_chain(&state);
    if status.registered {
        println!("already registered — nothing to do");
        return;
    }

    // GPU identity + measurement. `--gflops`/`--gpu-hash` let an operator skip
    // a probe that would contend with a running job, but a fingerprint is
    // mandatory for the free path.
    let (gpu_hash, gpu_count, gflops) =
        if !args.entry && args.gpu_hash.is_none() && args.gflops.is_none() {
            // Legacy paid path never used a fingerprint; keep it that way.
            (
                [0u8; 32],
                args.gpu_count.unwrap_or(1),
                args.gflops.unwrap_or(0),
            )
        } else {
            match onboard::probe_local_gpu_with(args.gflops, args.gpu_hash) {
                Ok(g) => {
                    println!(
                        "  GPU:      {} (fingerprint {}, {} GFLOPS measured{})",
                        g.gpu_name,
                        if g.weak_fingerprint {
                            "weak (no UUID)"
                        } else {
                            "strong (UUID)"
                        },
                        g.compute_gflops,
                        if g.gflops_was_override {
                            " (override)"
                        } else {
                            ""
                        }
                    );
                    (
                        g.gpu_info_hash,
                        args.gpu_count.unwrap_or(g.gpu_count.max(1)),
                        g.compute_gflops,
                    )
                }
                Err(e) => {
                    if args.entry {
                        eprintln!("gpn_register: {e}");
                        std::process::exit(2);
                    }
                    println!("  GPU probe unavailable ({e}); using paid path parameters");
                    ([0u8; 32], args.gpu_count.unwrap_or(1), 0)
                }
            }
        };

    let params = EntryParams {
        challenge_response: state.challenge,
        gpu_info_hash: gpu_hash,
        gpu_count,
        compute_gflops: gflops,
        requirements: ProviderRequirements::default(),
    };

    let miner = SharedSealer::default();
    let mut log = |line: &str| println!("  {line}");
    let outcome = onboard::run_entry(&chain, &miner, &account, &params, args.entry, &mut log).await;

    println!("{}", outcome.summary());
    if outcome.succeeded() {
        if matches!(outcome, EntryOutcome::Registered) {
            println!(
                "  the account paid 0 coins — the entry was bought with computation. \
                 Start the node with `gpn_node` to begin serving."
            );
        }
    } else {
        std::process::exit(2);
    }
}
