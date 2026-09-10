//! gpn_treasury — CLI for the on-chain vested treasury (`pallet-gpn-treasury`).
//!
//! The treasury allocation (e.g. 200M GPN) lives in the keyless `gpn/trsy`
//! **pallet account** (`PalletId(*b"gpn/trsy")`). Nobody holds a private key for
//! it; funds move ONLY through the pallet's 13-of-25 governance flow, bounded by
//! the unlocked vesting tranches. This tool wraps those extrinsics and the
//! read-only state so an operator can run the treasury day-to-day.
//!
//! Usage:
//!   gpn_treasury <command> [args] [--endpoint ws://127.0.0.1:9944] [--signer //Alice]
//!
//! Read-only:
//!   status                     treasury account, balance, unlocked, spendable, threshold, roster size
//!   schedule                   the vesting tranches (unlock block, amount, unlocked?)
//!   proposals                  list spend proposals (id, dest, amount, approvals, executed)
//!   roster                     list the governance roster addresses
//!
//! Signed (each must be a roster member):
//!   propose <dest-ss58> <amount-gpn>          create a spend proposal (auto-approves as proposer)
//!   approve <proposal-id>                     approve a pending proposal
//!   execute <proposal-id>                     execute once >= threshold approvals & within unlocked
//!
//! Notes:
//!   - Amounts are in WHOLE GPN (converted to 12-decimal base units).
//!   - `--signer` is a Substrate URI (e.g. `//Alice`, or a real seed phrase for
//!     production roster members). The signer must be in the roster.

use gpn_agent::chain::signer;
use gpn_agent::utils::crypto;
use subxt::dynamic::Value;
use subxt::tx::Signer as _;

const BASE: u128 = 1_000_000_000_000; // 12 decimals

fn gpn_to_base(gpn: u128) -> u128 {
    gpn.saturating_mul(BASE)
}

struct Opts {
    endpoint: String,
    signer_uri: String,
    command: String,
    args: Vec<String>,
}

fn parse_args() -> Opts {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut endpoint = "ws://127.0.0.1:9944".to_string();
    let mut signer_uri = "//Alice".to_string();
    let mut positional = Vec::new();
    let mut it = raw.into_iter().peekable();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--endpoint" => endpoint = it.next().unwrap_or(endpoint),
            "--signer" => signer_uri = it.next().unwrap_or(signer_uri),
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            _ => positional.push(a),
        }
    }
    let command = positional.first().cloned().unwrap_or_else(|| "status".to_string());
    let args = positional.into_iter().skip(1).collect();
    Opts { endpoint, signer_uri, command, args }
}

fn print_help() {
    eprintln!(
        "gpn_treasury — vested treasury CLI\n\
         \n\
         usage: gpn_treasury <command> [args] [--endpoint <ws-url>] [--signer <uri>]\n\
         \n\
         commands:\n\
           status                          treasury account, balance, unlocked, spendable, threshold\n\
           schedule                        vesting tranches\n\
           proposals                       list spend proposals\n\
           roster                          list governance roster addresses\n\
           propose <dest> <amount-gpn>     create a spend proposal\n\
           approve <proposal-id>           approve a pending proposal (must be roster member)\n\
           execute <proposal-id>           execute an approved proposal\n"
    );
}

// ── Reads (via ChainClient) ────────────────────────────────────────────────


async fn cmd_status(endpoint: &str) {
    let chain = match gpn_agent::chain::ChainClient::new(endpoint).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cannot connect to {endpoint}: {e}");
            return;
        }
    };
    let acct = chain.treasury_account();
    println!("Treasury pallet account: {acct}");
    match chain.treasury_balance().await {
        Some(b) => println!("Balance:        {} GPN ({} base units)", b / BASE, b),
        None => println!("Balance:        (unavailable — is a node running at {endpoint}?)"),
    }
    let thr = chain.treasury_threshold().await;
    let roster = chain.treasury_roster_len().await;
    match (thr, roster) {
        (Some(t), Some(r)) => println!("Governance:     {t}-of-{r} approvals required"),
        _ => println!("Governance:     (unavailable)"),
    }
    if let Some(rel) = chain.treasury_total_released().await {
        println!("Total released: {} GPN", rel / BASE);
    }
    // Vesting snapshot: unlocked vs spendable.
    if let (Some(sched), Some(head)) = (chain.treasury_schedule().await, chain.head_number().await) {
        let unlocked: u128 = sched.iter().filter(|(b, _)| head >= *b).map(|(_, a)| *a).sum();
        let rel = chain.treasury_total_released().await.unwrap_or(0);
        let bal = chain.treasury_balance().await.unwrap_or(0);
        println!("Head block:     {head}");
        println!("Unlocked:       {} GPN", unlocked / BASE);
        println!("Spendable:      {} GPN (unlocked - released)", unlocked.saturating_sub(rel) / BASE);
        println!("Allocation:     {} GPN (tranches: {})", sched.iter().map(|(_, a)| *a).sum::<u128>() / BASE, sched.len());
        let _ = bal;
    }
}

async fn cmd_roster(endpoint: &str) {
    let chain = match gpn_agent::chain::ChainClient::new(endpoint).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cannot connect to {endpoint}: {e}");
            return;
        }
    };
    match chain.treasury_roster_len().await {
        Some(n) => {
            println!("Governance roster: {n} addresses");
            println!("Threshold: {} approvals", chain.treasury_threshold().await.unwrap_or(0));
            println!("\n(Individual roster addresses — view the full list via");
            println!(" polkadot.js.org/apps → Developer → Chain State → gpnTreasury.roster)");
        }
        None => println!("Roster unavailable — is a node running at {endpoint}?"),
    }
}

async fn cmd_schedule(endpoint: &str) {
    let chain = match gpn_agent::chain::ChainClient::new(endpoint).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cannot connect to {endpoint}: {e}");
            return;
        }
    };
    let head = chain.head_number().await.unwrap_or(0);
    match chain.treasury_schedule().await {
        Some(sched) => {
            println!("Vesting schedule (head block {head}):");
            println!("  {:<14} {:>18}  {}", "unlock block", "amount (GPN)", "status");
            for (b, a) in &sched {
                let status = if head >= *b { "UNLOCKED" } else { "locked" };
                println!("  {:<14} {:>18}  {}", b, a / BASE, status);
            }
            let total: u128 = sched.iter().map(|(_, a)| *a).sum();
            println!("  total = {} GPN over {} tranches", total / BASE, sched.len());
        }
        None => println!("Schedule unavailable — is a node running at {endpoint}?"),
    }
}

async fn cmd_proposals(endpoint: &str) {
    let chain = match gpn_agent::chain::ChainClient::new(endpoint).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cannot connect to {endpoint}: {e}");
            return;
        }
    };
    match chain.treasury_threshold().await {
        Some(t) => {
            println!("Spend proposals require {t} approvals.");
            println!("\nProposals are stored on-chain (GPNTreasury.ProposalDest/Amount/");
            println!("Approvals/Executed). View the live list via polkadot.js.org/apps →");
            println!("Developer → Chain State → gpnTreasury, or propose one with:\n");
            println!("  gpn_treasury propose <dest-ss58> <amount-gpn> --signer <roster-uri>");
        }
        None => println!("Unavailable — is a node running at {endpoint}?"),
    }
}

// ── Signed writes ──────────────────────────────────────────────────────────

async fn signed_call(
    opts: &Opts,
    call_name: &str,
    args: Vec<Value>,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = signer::connect(&opts.endpoint).await?;
    let signer = signer::Sr25519Signer::from_uri(&opts.signer_uri)?;
    let call = subxt::dynamic::tx("GPNTreasury", call_name, args);
    let nonce = signer::current_nonce(&client, &opts.endpoint, &signer.account_id()).await;
    signer::submit_signed(&client, &call, &signer, nonce).await?;
    Ok(())
}

async fn cmd_propose(opts: &Opts) -> Result<(), Box<dyn std::error::Error>> {
    let dest = opts.args.get(0).ok_or("usage: propose <dest-ss58> <amount-gpn>")?;
    let amount: u128 = opts
        .args
        .get(1)
        .ok_or("usage: propose <dest-ss58> <amount-gpn>")?
        .parse()
        .map_err(|_| "amount must be a whole-GPN integer")?;
    let dest32: subxt::utils::AccountId32 = dest.parse()?;
    let key = (dest32.as_ref() as &[u8; 32]).to_vec();
    let base = gpn_to_base(amount);
    println!(
        "Proposing spend of {} GPN to {} as {}...",
        amount, dest, opts.signer_uri
    );
    signed_call(
        opts,
        "propose_spend",
        vec![Value::from_bytes(key), Value::u128(base)],
    )
    .await?;
    println!("✅ propose_spend submitted (as proposer you count as approval 1/13).");
    Ok(())
}

async fn cmd_approve(opts: &Opts) -> Result<(), Box<dyn std::error::Error>> {
    let id: u64 = opts
        .args
        .get(0)
        .ok_or("usage: approve <proposal-id>")?
        .parse()
        .map_err(|_| "proposal id must be an integer")?;
    println!("Approving proposal {} as {}...", id, opts.signer_uri);
    signed_call(opts, "approve_spend", vec![Value::u128(id as u128)]).await?;
    println!("✅ approve_spend submitted.");
    Ok(())
}

async fn cmd_execute(opts: &Opts) -> Result<(), Box<dyn std::error::Error>> {
    let id: u64 = opts
        .args
        .get(0)
        .ok_or("usage: execute <proposal-id>")?
        .parse()
        .map_err(|_| "proposal id must be an integer")?;
    println!("Executing proposal {} as {}...", id, opts.signer_uri);
    signed_call(opts, "execute_spend", vec![Value::u128(id as u128)]).await?;
    println!("✅ execute_spend submitted (fails on-chain if <13 approvals or amount exceeds unlocked).");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let _ = crypto::hash_bytes(b"gpn"); // keep utils link (no-op)
    let opts = parse_args();

    match opts.command.as_str() {
        "status" => cmd_status(&opts.endpoint).await,
        "schedule" => cmd_schedule(&opts.endpoint).await,
        "proposals" => cmd_proposals(&opts.endpoint).await,
        "roster" => cmd_roster(&opts.endpoint).await,
        "propose" => cmd_propose(&opts).await?,
        "approve" => cmd_approve(&opts).await?,
        "execute" => cmd_execute(&opts).await?,
        other => {
            eprintln!("unknown command '{other}'");
            print_help();
            std::process::exit(1);
        }
    }
    Ok(())
}
